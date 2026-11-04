//! Now-playing information and transport controls through Windows' media session API.
//!
//! All WinRT calls happen on one worker thread. Session events only enqueue a
//! refresh; the worker reads the new state, stores it in `MediaState`, and posts
//! `msg::MEDIA` to the notch window.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use windows::Foundation::TypedEventHandler;
use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSession as Session,
    GlobalSystemMediaTransportControlsSessionManager as SessionManager,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus as PlaybackStatus,
};
use windows::Win32::System::SystemInformation::GetSystemTimePreciseAsFileTime;
use windows::Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize};
use windows::core::{Result, RuntimeType};

use windows::Storage::Streams::IRandomAccessStreamReference;

use crate::art::{self, Art};
use crate::lyrics::{self, Lyrics, Query};
use crate::msg::{self, WindowHandle};

/// How long to wait for a track's duration before searching lyrics without it.
const DURATION_GRACE: Duration = Duration::from_millis(1200);
/// A position report further than this from the extrapolated position is a seek.
const POSITION_TOLERANCE_MS: u64 = 600;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Track {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration_ms: u64,
}

#[derive(Debug)]
pub struct MediaState {
    pub track: Option<Track>,
    pub playing: bool,
    position_ms: u64,
    sampled_at: Instant,
    /// Playback speed: 1.0 normally, 2.0 at double speed.
    rate: f64,
    pub lyrics: Option<Lyrics>,
    /// The track the current `lyrics` (or the fetch in flight) belong to.
    lyrics_track: Option<(String, String)>,
    pub art: Option<Arc<Art>>,
    /// Changes whenever `art` does, so the UI knows to rebuild its bitmap.
    pub art_id: u64,
    /// The track `art` was read for.
    art_track: Option<(String, String)>,
    /// Windows' app id for the player (`Spotify.exe`, `Chrome`, ...), or empty.
    pub source_app: String,
}

impl Default for MediaState {
    fn default() -> Self {
        Self {
            track: None,
            playing: false,
            position_ms: 0,
            sampled_at: Instant::now(),
            rate: 1.0,
            lyrics: None,
            lyrics_track: None,
            art: None,
            art_id: 0,
            art_track: None,
            source_app: String::new(),
        }
    }
}

impl MediaState {
    /// Playback position extrapolated to now.
    pub fn position_ms(&self) -> u64 {
        let mut position = self.position_ms;
        if self.playing {
            let elapsed = self.sampled_at.elapsed().as_millis() as u64;
            position = advance(position, elapsed, self.rate);
        }
        match &self.track {
            Some(track) if track.duration_ms > 0 => position.min(track.duration_ms),
            _ => position,
        }
    }
}

pub type SharedState = Arc<Mutex<MediaState>>;

pub fn lock(state: &SharedState) -> std::sync::MutexGuard<'_, MediaState> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

enum Command {
    SessionChanged,
    /// `properties` is set when the title, artist or artwork may have changed.
    Refresh {
        properties: bool,
    },
    TogglePlayPause,
    Next,
    Previous,
    Quit,
}

pub struct Media {
    commands: Sender<Command>,
    pub state: SharedState,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl Media {
    pub fn start(window: WindowHandle, fetch_lyrics: Arc<AtomicBool>) -> Self {
        let (commands, inbox) = mpsc::channel();
        let state = SharedState::default();
        let worker = {
            let commands = commands.clone();
            let state = state.clone();
            std::thread::Builder::new()
                .name("media".into())
                .spawn(move || {
                    let mut worker = Worker {
                        window,
                        state,
                        commands,
                        fetch_lyrics,
                        session: None,
                        cache: Arc::default(),
                        fetch_gate: Arc::default(),
                        pending_fetch: None,
                    };
                    // Without the media API (stripped-down Windows editions) the
                    // notch still works; it just never shows a track.
                    let _ = worker.run(inbox);
                })
                .ok()
        };
        Self {
            commands,
            state,
            worker,
        }
    }

    pub fn toggle_play_pause(&self) {
        let _ = self.commands.send(Command::TogglePlayPause);
    }

    pub fn next(&self) {
        let _ = self.commands.send(Command::Next);
    }

    pub fn previous(&self) {
        let _ = self.commands.send(Command::Previous);
    }

    /// Re-reads the state, e.g. after lyric fetching was switched back on.
    pub fn refresh(&self) {
        let _ = self.commands.send(Command::Refresh { properties: false });
    }
}

impl Drop for Media {
    fn drop(&mut self) {
        // Not joined: the worker may be stuck in a call to an unresponsive player,
        // and waiting for it would freeze exit. The process ending stops it anyway.
        let _ = self.commands.send(Command::Quit);
        drop(self.worker.take());
    }
}

struct AttachedSession {
    session: Session,
    tokens: [i64; 3],
}

type LyricsCache = Arc<Mutex<HashMap<Query, Lyrics>>>;

/// Tracks whose lyrics are kept in memory; past this the cache starts over.
const MAX_CACHED: usize = 200;
/// Approximate text bytes the cache may hold; one song can reach 2000 lines of
/// 300 chars, so the entry count alone does not bound memory.
const MAX_CACHED_BYTES: usize = 8 * 1024 * 1024;
/// Wait after the first failed fetch of a query; doubles per failure up to the max.
const BACKOFF_START: Duration = Duration::from_secs(30);
const BACKOFF_MAX: Duration = Duration::from_secs(600);
/// Fetches running at once. A skipped track's fetch finishes in the background
/// (its result is dropped), so a few may overlap while the user skips around.
const MAX_IN_FLIGHT: usize = 3;
/// When that many are running, the next one is tried again after this.
const BUSY_RETRY: Duration = Duration::from_secs(1);

/// Approximate memory held by one entry: the line text.
fn lyrics_bytes(lyrics: &Lyrics) -> usize {
    match lyrics {
        Lyrics::Synced(lines) => lines.iter().map(|line| line.text.len()).sum(),
        _ => 0,
    }
}

/// Adds an entry, starting the cache over first when it would pass either limit.
fn cache_insert(cache: &mut HashMap<Query, Lyrics>, query: Query, lyrics: Lyrics) {
    let held: usize = cache.values().map(lyrics_bytes).sum();
    if cache.len() >= MAX_CACHED || held + lyrics_bytes(&lyrics) > MAX_CACHED_BYTES {
        cache.clear();
    }
    cache.insert(query, lyrics);
}

/// What the gate says about fetching a query now.
#[derive(Debug, PartialEq)]
enum Gate {
    /// Fetch it; it is now in flight.
    Go,
    /// It is already being fetched; that result will arrive.
    Running,
    /// Not now: ask again at this time. The caller keeps the request, so a
    /// track's lyrics are never given up on, only delayed.
    RetryAt(Instant),
}

/// Decides when a lyrics fetch may start: a few at a time, never the same query
/// twice, and a query that just failed waits out a growing delay so an offline
/// network doesn't cost a thread per refresh.
#[derive(Default)]
struct FetchGate {
    in_flight: Vec<Query>,
    failed: Option<(Query, Instant)>,
    /// Delay applied to the next failure; zero until one happens.
    delay: Duration,
}

impl FetchGate {
    fn try_begin(&mut self, query: &Query, now: Instant) -> Gate {
        if self.in_flight.contains(query) {
            return Gate::Running;
        }
        if let Some((failed, not_before)) = &self.failed
            && failed == query
            && now < *not_before
        {
            return Gate::RetryAt(*not_before);
        }
        if self.in_flight.len() >= MAX_IN_FLIGHT {
            return Gate::RetryAt(now + BUSY_RETRY);
        }
        self.in_flight.push(query.clone());
        Gate::Go
    }

    fn finish(&mut self, query: &Query, succeeded: bool, now: Instant) {
        if let Some(index) = self.in_flight.iter().position(|q| q == query) {
            self.in_flight.swap_remove(index);
        }
        if succeeded {
            self.failed = None;
            self.delay = Duration::ZERO;
        } else {
            let delay = self.delay.max(BACKOFF_START);
            self.failed = Some((query.clone(), now + delay));
            self.delay = (delay * 2).min(BACKOFF_MAX);
        }
    }
}

/// Reports the outcome to the gate when the fetch thread ends, panics included.
struct FetchGuard {
    gate: Arc<Mutex<FetchGate>>,
    query: Query,
    succeeded: bool,
}

impl Drop for FetchGuard {
    fn drop(&mut self) {
        let mut gate = self.gate.lock().unwrap_or_else(PoisonError::into_inner);
        gate.finish(&self.query, self.succeeded, Instant::now());
    }
}
/// Longest title, artist or album kept. Real ones are far shorter; a player
/// could send megabytes.
const MAX_TEXT_CHARS: usize = 300;

fn clip(text: String) -> String {
    match text.char_indices().nth(MAX_TEXT_CHARS) {
        Some((cut, _)) => text[..cut].to_owned(),
        None => text,
    }
}

struct Worker {
    window: WindowHandle,
    state: SharedState,
    commands: Sender<Command>,
    fetch_lyrics: Arc<AtomicBool>,
    session: Option<AttachedSession>,
    cache: LyricsCache,
    fetch_gate: Arc<Mutex<FetchGate>>,
    /// A track whose lyrics haven't been requested yet, and when to request them:
    /// a moment after it appeared (its duration may still be on its way), or
    /// when the fetch gate said to try again.
    pending_fetch: Option<(Track, Instant)>,
}

impl Worker {
    fn run(&mut self, inbox: Receiver<Command>) -> Result<()> {
        unsafe { RoInitialize(RO_INIT_MULTITHREADED)? };
        let manager = SessionManager::RequestAsync()?.join()?;
        let notify = self.commands.clone();
        let token = manager.CurrentSessionChanged(&TypedEventHandler::new(move |_, _| {
            let _ = notify.send(Command::SessionChanged);
            Ok(())
        }))?;
        self.attach(&manager);

        // A command that arrived while coalescing refreshes, handled next.
        let mut deferred = None;
        loop {
            let command = if let Some(command) = deferred.take() {
                command
            } else {
                match self.pending_fetch.as_ref() {
                    Some((_, due)) => {
                        match inbox.recv_timeout(due.saturating_duration_since(Instant::now())) {
                            Ok(command) => command,
                            Err(RecvTimeoutError::Timeout) => {
                                self.fire_pending_fetch();
                                continue;
                            }
                            Err(RecvTimeoutError::Disconnected) => break,
                        }
                    }
                    None => match inbox.recv() {
                        Ok(command) => command,
                        Err(_) => break,
                    },
                }
            };
            match command {
                Command::SessionChanged => self.attach(&manager),
                Command::Refresh { mut properties } => {
                    // Players fire several events per track change; read once. Anything
                    // else found while draining (a click, Quit) must not be lost.
                    loop {
                        match inbox.try_recv() {
                            Ok(Command::Refresh { properties: more }) => properties |= more,
                            Ok(other) => {
                                deferred = Some(other);
                                break;
                            }
                            Err(_) => break,
                        }
                    }
                    self.refresh(properties);
                }
                Command::TogglePlayPause => self.control(|s| s.TryTogglePlayPauseAsync()),
                Command::Next => self.control(|s| s.TrySkipNextAsync()),
                Command::Previous => self.control(|s| s.TrySkipPreviousAsync()),
                Command::Quit => break,
            }
        }

        self.detach();
        let _ = manager.RemoveCurrentSessionChanged(token);
        Ok(())
    }

    fn control(&self, action: impl Fn(&Session) -> Result<windows_future::IAsyncOperation<bool>>) {
        if let Some(attached) = &self.session {
            let _ = action(&attached.session).and_then(|operation| operation.join());
        }
    }

    fn attach(&mut self, manager: &SessionManager) {
        self.detach();
        if let Ok(session) = manager.GetCurrentSession() {
            let tokens = (
                session.MediaPropertiesChanged(&refresh_on(&self.commands, true)),
                session.PlaybackInfoChanged(&refresh_on(&self.commands, false)),
                session.TimelinePropertiesChanged(&refresh_on(&self.commands, false)),
            );
            if let (Ok(a), Ok(b), Ok(c)) = tokens {
                self.session = Some(AttachedSession {
                    session,
                    tokens: [a, b, c],
                });
            }
        }
        self.refresh(true);
    }

    fn detach(&mut self) {
        if let Some(attached) = self.session.take() {
            let [a, b, c] = attached.tokens;
            let _ = attached.session.RemoveMediaPropertiesChanged(a);
            let _ = attached.session.RemovePlaybackInfoChanged(b);
            let _ = attached.session.RemoveTimelinePropertiesChanged(c);
        }
    }

    fn refresh(&mut self, properties_changed: bool) {
        let snapshot = self
            .session
            .as_ref()
            .and_then(|attached| read_session(&attached.session).ok());
        // Artwork is decoded only when the player says its properties changed or the
        // track is new, never on the frequent timeline reports.
        let art = snapshot.as_ref().and_then(|snapshot| {
            let key = (snapshot.track.title.clone(), snapshot.track.artist.clone());
            let stale = lock(&self.state).art_track.as_ref() != Some(&key);
            (properties_changed || stale).then(|| {
                let art = snapshot
                    .thumbnail
                    .as_ref()
                    .and_then(|thumb| art::decode(thumb).ok());
                (key, art.map(Arc::new))
            })
        });
        let mut state = lock(&self.state);
        let mut changed = true;
        if let Some((key, art)) = art {
            state.art_track = Some(key);
            state.art = art;
            state.art_id += 1;
        }
        match snapshot {
            Some(snapshot) => {
                // Browsers report the timeline every second or so. When the position is
                // just where it was expected to be, there is nothing new to show.
                let drift = snapshot.position_ms.abs_diff(state.position_ms());
                changed = state.track.as_ref() != Some(&snapshot.track)
                    || properties_changed
                    || state.playing != snapshot.playing
                    || drift > POSITION_TOLERANCE_MS;
                state.playing = snapshot.playing;
                state.source_app = snapshot.source_app;
                state.position_ms = snapshot.position_ms;
                state.sampled_at = Instant::now();
                state.rate = snapshot.rate;
                let key = (snapshot.track.title.clone(), snapshot.track.artist.clone());
                if state.lyrics_track.as_ref() != Some(&key) {
                    state.lyrics_track = Some(key);
                    state.lyrics = None;
                    self.pending_fetch =
                        Some((snapshot.track.clone(), Instant::now() + DURATION_GRACE));
                } else if let Some((pending, _)) = &mut self.pending_fetch {
                    // Players often publish the duration a moment after the title.
                    pending.duration_ms = snapshot.track.duration_ms;
                    pending.album = snapshot.track.album.clone();
                }
                state.track = Some(snapshot.track);
            }
            None => {
                let art_id = state.art_id + 1;
                *state = MediaState {
                    art_id,
                    ..MediaState::default()
                };
                self.pending_fetch = None;
            }
        }
        drop(state);

        if self
            .pending_fetch
            .as_ref()
            .is_some_and(|(track, _)| track.duration_ms > 0)
        {
            self.fire_pending_fetch();
        }
        if changed {
            self.window.post(msg::MEDIA, 0, 0);
        }
    }

    fn fire_pending_fetch(&mut self) {
        let Some((track, _)) = self.pending_fetch.take() else {
            return;
        };
        if !self.fetch_lyrics.load(Ordering::Relaxed) {
            // Forget which track this was, so switching downloads back on fetches it.
            lock(&self.state).lyrics_track = None;
            return;
        }
        let query = Query {
            title: track.title.clone(),
            artist: track.artist.clone(),
            album: track.album.clone(),
            duration_secs: (track.duration_ms > 0)
                .then(|| u32::try_from((track.duration_ms + 500) / 1000).unwrap_or(u32::MAX)),
        };
        let cached = self
            .cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&query)
            .cloned();
        if let Some(lyrics) = cached {
            store_lyrics(&self.state, self.window, &query, lyrics);
            return;
        }

        let gate = self
            .fetch_gate
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .try_begin(&query, Instant::now());
        match gate {
            Gate::Go => {}
            // Its fetch is still finishing. If that was for an earlier visit of this
            // track its result may have been dropped, so look again shortly: the
            // cache or the gate will have the answer by then.
            Gate::Running => {
                self.pending_fetch = Some((track, Instant::now() + BUSY_RETRY));
                return;
            }
            // Too many running, or this query failed a moment ago: keep the request
            // and come back to it. Players often send nothing more until the next
            // track, so waiting for another refresh could mean no lyrics at all.
            Gate::RetryAt(due) => {
                self.pending_fetch = Some((track, due));
                return;
            }
        }

        let (state, window, cache) = (self.state.clone(), self.window, self.cache.clone());
        let commands = self.commands.clone();
        let guard = FetchGuard {
            gate: self.fetch_gate.clone(),
            query: query.clone(),
            succeeded: false,
        };
        let _ = std::thread::Builder::new()
            .name("lyrics".into())
            .spawn(move || {
                // Capture the whole guard (not just a field), so it drops with the thread.
                let mut guard = guard;
                unsafe {
                    let _ = RoInitialize(RO_INIT_MULTITHREADED);
                }
                match lyrics::fetch(&query) {
                    Ok(lyrics) => {
                        guard.succeeded = true;
                        let mut cached = cache.lock().unwrap_or_else(PoisonError::into_inner);
                        cache_insert(&mut cached, query.clone(), lyrics.clone());
                        drop(cached);
                        store_lyrics(&state, window, &query, lyrics);
                    }
                    // Not cached, and the track is forgotten. The refresh asked for
                    // here notices that, and the gate schedules the retry after
                    // its backoff; it can't come from the player, which may send
                    // nothing more for this track.
                    Err(_) => {
                        let mut state = lock(&state);
                        if state.lyrics_track.as_ref()
                            == Some(&(query.title.clone(), query.artist.clone()))
                        {
                            state.lyrics_track = None;
                            drop(state);
                            // The guard must report the failure before the refresh
                            // asks the gate, or it would still see this one running.
                            drop(guard);
                            let _ = commands.send(Command::Refresh { properties: false });
                        }
                    }
                }
            });
    }
}

/// An event handler that just asks the worker to re-read the session.
fn refresh_on<T, U>(commands: &Sender<Command>, properties: bool) -> TypedEventHandler<T, U>
where
    T: RuntimeType + 'static,
    U: RuntimeType + 'static,
{
    let commands = commands.clone();
    TypedEventHandler::new(move |_, _| {
        let _ = commands.send(Command::Refresh { properties });
        Ok(())
    })
}

/// Stores lyrics only if the user is still on the track they were fetched for.
fn store_lyrics(state: &SharedState, window: WindowHandle, query: &Query, lyrics: Lyrics) {
    let mut state = lock(state);
    if state.lyrics_track.as_ref() == Some(&(query.title.clone(), query.artist.clone())) {
        state.lyrics = Some(lyrics);
        drop(state);
        window.post(msg::LYRICS, 0, 0);
    }
}

struct Snapshot {
    track: Track,
    playing: bool,
    position_ms: u64,
    rate: f64,
    thumbnail: Option<IRandomAccessStreamReference>,
    source_app: String,
}

/// A player's timeline in 100 ns ticks, as it reported it. Any of these can be
/// anything a player sends, so the math saturates instead of wrapping.
struct Reported {
    start: i64,
    end: i64,
    position: i64,
    /// When the position was sampled (FILETIME), or 0 when it isn't advancing.
    updated: i64,
    /// Playback speed since then.
    rate: f64,
}

/// `position_ms` after `elapsed_ms` of playback at `rate`.
fn advance(position_ms: u64, elapsed_ms: u64, rate: f64) -> u64 {
    position_ms.saturating_add((elapsed_ms as f64 * rate) as u64)
}

/// The speed a player reports, or normal speed for nothing or nonsense.
fn playback_rate(reported: Option<f64>) -> f64 {
    reported
        .filter(|rate| rate.is_finite() && *rate > 0.0 && *rate <= 16.0)
        .unwrap_or(1.0)
}

impl Reported {
    /// Track length and current position in milliseconds, at `now` (FILETIME).
    fn to_ms(&self, now: i64) -> (u64, u64) {
        const TICKS_PER_MS: i64 = 10_000;
        let mut position = self.position.saturating_sub(self.start);
        if self.updated > 0 && now > self.updated {
            let elapsed = now.saturating_sub(self.updated) as f64 * self.rate;
            position = position.saturating_add(elapsed as i64);
        }
        let ms = |ticks: i64| (ticks.max(0) / TICKS_PER_MS) as u64;
        (ms(self.end.saturating_sub(self.start)), ms(position))
    }
}

fn read_session(session: &Session) -> Result<Snapshot> {
    let properties = session.TryGetMediaPropertiesAsync()?.join()?;
    let info = session.GetPlaybackInfo()?;
    let playing = info.PlaybackStatus()? == PlaybackStatus::Playing;
    let rate = playback_rate(info.PlaybackRate().ok().and_then(|rate| rate.Value().ok()));
    let timeline = session.GetTimelineProperties()?;

    let reported = Reported {
        start: timeline.StartTime()?.Duration,
        end: timeline.EndTime()?.Duration,
        position: timeline.Position()?.Duration,
        // The position was sampled when the player last reported it, not now.
        updated: if playing {
            timeline.LastUpdatedTime()?.UniversalTime
        } else {
            0
        },
        rate,
    };
    let now = unsafe { GetSystemTimePreciseAsFileTime() };
    let now = ((now.dwHighDateTime as i64) << 32) | now.dwLowDateTime as i64;
    let (duration_ms, position_ms) = reported.to_ms(now);

    Ok(Snapshot {
        track: Track {
            title: clip(properties.Title()?.to_string()),
            artist: clip(properties.Artist()?.to_string()),
            album: clip(properties.AlbumTitle()?.to_string()),
            duration_ms,
        },
        playing,
        position_ms,
        rate,
        thumbnail: properties.Thumbnail().ok(),
        source_app: session
            .SourceAppUserModelId()
            .map(|id| id.to_string())
            .unwrap_or_default(),
    })
}

#[cfg(test)]
mod clip_tests {
    use super::*;

    #[test]
    fn clip_should_keep_short_text_and_cut_long_text_on_a_character() {
        let long = "ع".repeat(MAX_TEXT_CHARS + 5);
        assert_eq!(
            (clip("Song".into()), clip(long).chars().count()),
            ("Song".to_owned(), MAX_TEXT_CHARS)
        );
    }
}

#[cfg(test)]
mod fetch_tests {
    use super::*;
    use crate::lyrics::Line;

    fn query(title: &str) -> Query {
        Query {
            title: title.into(),
            artist: "Artist".into(),
            album: String::new(),
            duration_secs: Some(200),
        }
    }

    fn synced(text_len: usize) -> Lyrics {
        Lyrics::Synced(vec![Line {
            time_ms: 0,
            text: "x".repeat(text_len),
        }])
    }

    #[test]
    fn a_failed_query_should_wait_out_the_backoff_but_other_tracks_should_not() {
        let (mut gate, now) = (FetchGate::default(), Instant::now());
        assert_eq!(gate.try_begin(&query("a"), now), Gate::Go);
        gate.finish(&query("a"), false, now);
        assert_eq!(
            gate.try_begin(&query("a"), now + BACKOFF_START - Duration::from_secs(1)),
            Gate::RetryAt(now + BACKOFF_START)
        );
        assert_eq!(gate.try_begin(&query("b"), now), Gate::Go);
        gate.finish(&query("b"), true, now);
        assert_eq!(gate.try_begin(&query("a"), now + BACKOFF_START), Gate::Go);
    }

    #[test]
    fn repeated_failures_should_double_the_delay_up_to_the_cap() {
        let (mut gate, mut now) = (FetchGate::default(), Instant::now());
        let mut expected = BACKOFF_START;
        for _ in 0..10 {
            assert_eq!(gate.try_begin(&query("a"), now), Gate::Go);
            gate.finish(&query("a"), false, now);
            let (_, not_before) = gate.failed.clone().unwrap();
            assert_eq!(not_before - now, expected);
            now = not_before;
            expected = (expected * 2).min(BACKOFF_MAX);
        }
        assert_eq!(expected, BACKOFF_MAX);
    }

    #[test]
    fn a_success_should_reset_the_backoff() {
        let (mut gate, now) = (FetchGate::default(), Instant::now());
        gate.try_begin(&query("a"), now);
        gate.finish(&query("a"), false, now);
        gate.try_begin(&query("b"), now);
        gate.finish(&query("b"), true, now);
        gate.try_begin(&query("a"), now);
        gate.finish(&query("a"), false, now);
        assert_eq!(gate.failed.map(|(_, at)| at - now), Some(BACKOFF_START));
    }

    #[test]
    fn a_new_track_should_fetch_while_the_previous_one_is_still_downloading() {
        let (mut gate, now) = (FetchGate::default(), Instant::now());
        assert_eq!(gate.try_begin(&query("a"), now), Gate::Go);
        assert_eq!(gate.try_begin(&query("b"), now), Gate::Go);
    }

    #[test]
    fn the_same_query_should_not_be_fetched_twice_at_once() {
        let (mut gate, now) = (FetchGate::default(), Instant::now());
        assert_eq!(gate.try_begin(&query("a"), now), Gate::Go);
        assert_eq!(gate.try_begin(&query("a"), now), Gate::Running);
    }

    #[test]
    fn past_the_limit_a_fetch_should_be_retried_shortly_not_dropped() {
        let (mut gate, now) = (FetchGate::default(), Instant::now());
        for n in 0..MAX_IN_FLIGHT {
            assert_eq!(gate.try_begin(&query(&n.to_string()), now), Gate::Go);
        }
        assert_eq!(
            gate.try_begin(&query("next"), now),
            Gate::RetryAt(now + BUSY_RETRY)
        );
        gate.finish(&query("0"), true, now);
        assert_eq!(gate.try_begin(&query("next"), now), Gate::Go);
    }

    #[test]
    fn the_cache_should_start_over_at_the_entry_limit() {
        let mut cache = HashMap::new();
        for n in 0..MAX_CACHED {
            cache_insert(&mut cache, query(&n.to_string()), Lyrics::NotFound);
        }
        assert_eq!(cache.len(), MAX_CACHED);
        cache_insert(&mut cache, query("next"), Lyrics::NotFound);
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn the_cache_should_start_over_at_the_byte_limit() {
        let mut cache = HashMap::new();
        let big = MAX_CACHED_BYTES / 2 + 1;
        cache_insert(&mut cache, query("a"), synced(big));
        cache_insert(&mut cache, query("b"), synced(100));
        assert_eq!(cache.len(), 2);
        cache_insert(&mut cache, query("c"), synced(big));
        assert_eq!(cache.len(), 1);
    }
}

#[cfg(test)]
mod timeline_tests {
    use super::*;

    #[test]
    fn to_ms_should_advance_at_the_playback_rate() {
        let reported = Reported {
            start: 0,
            end: 600_000_000,
            position: 10_000_000,
            updated: 1_000,
            rate: 2.0,
        };
        assert_eq!(reported.to_ms(1_000 + 20_000_000), (60_000, 5_000));
    }

    #[test]
    fn the_extrapolated_position_should_follow_the_rate() {
        assert_eq!(
            [
                advance(10_000, 2_000, 1.0),
                advance(10_000, 2_000, 1.5),
                advance(10_000, 2_000, 0.5)
            ],
            [12_000, 13_000, 11_000]
        );
    }

    #[test]
    fn a_rate_a_player_should_never_send_should_count_as_normal_speed() {
        assert_eq!(
            [
                playback_rate(Some(1.5)),
                playback_rate(None),
                playback_rate(Some(0.0)),
                playback_rate(Some(-2.0)),
                playback_rate(Some(f64::NAN)),
                playback_rate(Some(1e9)),
            ],
            [1.5, 1.0, 1.0, 1.0, 1.0, 1.0]
        );
    }

    #[test]
    fn to_ms_should_add_the_time_since_the_player_reported() {
        let reported = Reported {
            start: 0,
            end: 600_000_000,
            position: 10_000_000,
            updated: 1_000,
            rate: 1.0,
        };
        assert_eq!(reported.to_ms(1_000 + 20_000_000), (60_000, 3_000));
    }

    #[test]
    fn to_ms_should_not_wrap_on_extreme_values() {
        let reported = Reported {
            start: i64::MIN,
            end: i64::MAX,
            position: i64::MAX,
            updated: 1,
            rate: 1.0,
        };
        let (duration, position) = reported.to_ms(i64::MAX);
        assert_eq!(
            (duration, position),
            (i64::MAX as u64 / 10_000, i64::MAX as u64 / 10_000)
        );
    }

    #[test]
    fn to_ms_should_read_a_start_after_the_end_as_zero_length() {
        let reported = Reported {
            start: 100,
            end: 0,
            position: 0,
            updated: 0,
            rate: 1.0,
        };
        assert_eq!(reported.to_ms(0), (0, 0));
    }
}
