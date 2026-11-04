//! Lyrics from lrclib.net, and LRC parsing.
//!
//! BUKU sends LRCLIB the track title, artist, album and duration, nothing else.
//! The exact-match `/api/get` endpoint is tried first because the duration lets
//! LRCLIB pick the right version of a song; `/api/search` is the fallback.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde::de::DeserializeOwned;
use windows::Foundation::Uri;
use windows::Storage::Streams::{Buffer, DataReader, InputStreamOptions};
use windows::Web::Http::Filters::{
    HttpBaseProtocolFilter, HttpCacheReadBehavior, HttpCacheWriteBehavior, HttpCookieUsageBehavior,
};
use windows::Web::Http::{HttpClient, HttpCompletionOption};
use windows::core::{HSTRING, Result, RuntimeType};
use windows_future::{AsyncOperationWithProgressCompletedHandler, IAsyncOperationWithProgress};

/// Longest wait for the response headers.
const TIMEOUT: Duration = Duration::from_secs(8);
/// Longest a whole request may take, headers and body together, so a server
/// that trickles bytes cannot hold the lyrics thread.
const TOTAL_TIMEOUT: Duration = Duration::from_secs(15);
/// One song's lyrics are a few KB, but a search for a popular song returns every
/// version LRCLIB has (over 400 KB for some). Anything far bigger is not a
/// lyrics response.
const MAX_BODY_BYTES: u64 = 2 * 1024 * 1024;
const CHUNK_BYTES: u32 = 64 * 1024;
/// Waits before trying a request again after a "busy" answer.
const BUSY_PAUSES: [Duration; 2] = [Duration::from_secs(1), Duration::from_secs(2)];

/// Status codes that mean "try again shortly": rate limited, or the server or
/// its gateway overloaded.
fn is_busy(status: i32) -> bool {
    matches!(status, 429 | 502 | 503 | 504)
}
const USER_AGENT: &str = concat!("BUKU/", env!("CARGO_PKG_VERSION"));

#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    pub time_ms: u64,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Lyrics {
    Synced(Vec<Line>),
    Plain,
    Instrumental,
    NotFound,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Query {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration_secs: Option<u32>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Record {
    #[serde(default)]
    instrumental: bool,
    plain_lyrics: Option<String>,
    synced_lyrics: Option<String>,
    duration: Option<f64>,
}

impl Record {
    fn into_lyrics(self) -> Lyrics {
        if let Some(lines) = self.synced_lyrics.as_deref().map(parse_lrc)
            && !lines.is_empty()
        {
            return Lyrics::Synced(lines);
        }
        if self.instrumental {
            Lyrics::Instrumental
        } else if self
            .plain_lyrics
            .is_some_and(|text| !text.trim().is_empty())
        {
            Lyrics::Plain
        } else {
            Lyrics::NotFound
        }
    }
}

/// Blocking; call from a worker thread. Network errors are reported as `Err`
/// so the caller can retry later, while "no lyrics" is `Ok(NotFound)`.
///
/// Browsers report video titles such as `Artist - Song (Official Video) | Channel`.
/// When the title as reported finds nothing, a cleaned-up version is tried once.
pub fn fetch(query: &Query) -> Result<Lyrics> {
    if query.title.trim().is_empty() {
        return Ok(Lyrics::NotFound);
    }
    let client = HttpClient::Create(&private_filter()?)?;
    client
        .DefaultRequestHeaders()?
        .UserAgent()?
        .TryParseAdd(&HSTRING::from(USER_AGENT))?;

    // A decorated title or channel name (brackets, "- Topic", "Artist - Song")
    // is looked up cleaned first, since that is where songs are found. Then, for
    // "A - B" titles from a channel that isn't the artist, both halves both ways.
    // A lookup that fails never stops the next one, which may be the right one;
    // the failure counts only if none finds anything, so the fetch is retried.
    let mut lookups: Vec<(Query, Lookup)> = vec![(query.clone(), Lookup::Plain)];
    if let Some(cleaned) = clean_query(query) {
        // A channel name for the artist, or "Artist - Song" split out of the
        // title: the cleaned query is the real one, so it goes first. Otherwise
        // (a "(feat. X)" or "(Live)" in a player's own title) the original is
        // more exact and the cleaned one is the fallback.
        let index = usize::from(cleaned.artist == query.artist.trim());
        lookups.insert(index, (cleaned, Lookup::Plain));
    }
    for guess in split_guesses(query) {
        if !lookups.iter().any(|(tried, _)| *tried == guess) {
            lookups.push((guess, Lookup::Guess));
        }
    }
    let mut failure = None;
    let mut failures_in_a_row = 0;
    for (lookup, kind) in &lookups {
        match fetch_with(&client, lookup, *kind) {
            Ok(Lyrics::NotFound) => failures_in_a_row = 0,
            Ok(lyrics) => return Ok(lyrics),
            Err(err) => {
                failure = Some(err);
                failures_in_a_row += 1;
                // Offline or LRCLIB down: the rest would fail the same way, slowly.
                if failures_in_a_row == 2 {
                    break;
                }
            }
        }
    }
    failure.map_or(Ok(Lyrics::NotFound), Err)
}

/// How sure a lookup's title and artist are.
#[derive(Clone, Copy, PartialEq)]
enum Lookup {
    /// As the player reported them, or cleaned of decoration.
    Plain,
    /// One of two guesses at which half of "A - B" is the artist: only a record
    /// of the right length counts, so a wrong guess can't pick another song.
    Guess,
}

/// For a title with one ` - ` in it, the query with each half as the artist and
/// the other as the song. Empty when there is no such split, or no duration.
fn split_guesses(query: &Query) -> Vec<Query> {
    // A guess is only checked by its length; without one it could take any song.
    if query.duration_secs.is_none() {
        return Vec::new();
    }
    let title = strip_title(&query.title);
    let Some((left, right)) = title.split_once(" - ") else {
        return Vec::new();
    };
    let (left, right) = (left.trim(), right.trim());
    if left.is_empty() || right.is_empty() || right.contains(" - ") {
        return Vec::new();
    }
    let guess = |artist: &str, song: &str| Query {
        title: song.to_owned(),
        artist: artist.to_owned(),
        album: String::new(),
        duration_secs: query.duration_secs,
    };
    vec![guess(left, right), guess(right, left)]
}

fn fetch_with(client: &HttpClient, query: &Query, lookup: Lookup) -> Result<Lyrics> {
    // A guess has no album and rarely matches exactly; it goes straight to search.
    let exact = match query.duration_secs {
        Some(duration) if lookup == Lookup::Plain => {
            let url = format!(
                "https://lrclib.net/api/get?track_name={}&artist_name={}&album_name={}&duration={}",
                escape(&query.title)?,
                escape(&query.artist)?,
                escape(&query.album)?,
                duration
            );
            get(client, &url, Busy::MeansAbsent)
                .and_then(read_json::<Record>)
                .map(|record| record.map_or(Lyrics::NotFound, Record::into_lyrics))
        }
        _ => Ok(Lyrics::NotFound),
    };
    if let Ok(lyrics) = &exact
        && *lyrics != Lyrics::NotFound
    {
        return exact;
    }

    let url = format!(
        "https://lrclib.net/api/search?track_name={}&artist_name={}",
        escape(&query.title)?,
        escape(&query.artist)?
    );
    let records: Vec<Record> = read_json(get(client, &url, Busy::Retry)?)?.unwrap_or_default();
    let found = pick_best(records, query.duration_secs, lookup == Lookup::Guess);
    // The exact lookup failing for real (a timeout, a server error) while the
    // search found nothing must not be remembered as "no lyrics".
    match exact {
        // Plain lyrics from the search aren't kept either: the exact lookup may
        // have had the synced ones. "Instrumental" settles it either way.
        Err(err) if matches!(found, Lyrics::NotFound | Lyrics::Plain) => Err(err),
        _ => Ok(found),
    }
}

/// The HTTP settings for every lookup: nothing kept on disk (Windows' default
/// web cache would keep each lookup URL, and with it the title played), no
/// cookies, no redirects and no sign-in prompts. `docs/PRIVACY.md` promises the
/// lyrics stay in memory.
fn private_filter() -> Result<HttpBaseProtocolFilter> {
    let filter = HttpBaseProtocolFilter::new()?;
    let cache = filter.CacheControl()?;
    cache.SetReadBehavior(HttpCacheReadBehavior::NoCache)?;
    cache.SetWriteBehavior(HttpCacheWriteBehavior::NoCache)?;
    filter.SetCookieUsageBehavior(HttpCookieUsageBehavior::NoCookies)?;
    filter.SetAllowAutoRedirect(false)?;
    filter.SetAllowUI(false)?;
    Ok(filter)
}

/// A response body read as JSON. A body that is not JSON (a proxy's or a
/// maintenance page) is an error, so the fetch is retried later instead of the
/// track being remembered as having no lyrics.
fn read_json<T: DeserializeOwned>(body: Option<String>) -> Result<Option<T>> {
    body.map(|body| serde_json::from_str(&body).map_err(|_| windows::core::Error::empty()))
        .transpose()
}

/// Tags video titles carry after the song's name, outside any brackets.
const VIDEO_TAGS: [&str; 11] = [
    "official music video",
    "official video",
    "official audio",
    "music video",
    "lyric video",
    "lyrics",
    "audio",
    "hd",
    "hq",
    "4k",
    "8k",
];

/// `text` without video tags at its end ("Hit 'Em Up HD" is "Hit 'Em Up").
/// A tag only counts as whole words, and a title that is nothing but a tag
/// keeps it.
fn trim_video_tags(text: &str) -> &str {
    let mut text = text.trim();
    'strip: loop {
        for tag in VIDEO_TAGS {
            // Compared on the original's own ending (tags are ASCII), so a
            // character that changes length when lowercased can't shift the cut.
            let Some(cut) = text.len().checked_sub(tag.len()) else {
                continue;
            };
            if !text.is_char_boundary(cut) || !text[cut..].eq_ignore_ascii_case(tag) {
                continue;
            }
            let rest = &text[..cut];
            let kept = rest.trim_end_matches([' ', '-']);
            if rest.len() > kept.len() && !kept.is_empty() {
                text = kept;
                continue 'strip;
            }
        }
        return text;
    }
}

/// Video-title decoration removed: anything after `|`, text in brackets, a
/// `- Topic` or `VEVO` channel suffix, video tags like "HD", and extra spaces.
/// A spaced en or em dash ("Artist – Song") becomes a hyphen, the separator
/// the rest of the lookup splits on.
fn strip_decoration(text: &str) -> String {
    let text = text.split('|').next().unwrap_or_default();
    let mut out = String::with_capacity(text.len());
    let mut depth = 0u32;
    for c in text.chars() {
        match c {
            // Japanese and Chinese titles write brackets full width.
            '(' | '[' | '【' | '（' | '［' => depth += 1,
            ')' | ']' | '】' | '）' | '］' => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    // Dashes are read once brackets are gone and spaces are plain, so a dash
    // between no-break spaces, or one a removed bracket left spaced, splits too.
    // Word by word, so dashes next to each other ("A – – B") all change at once.
    let out = out
        .split_whitespace()
        .map(|word| match word {
            "\u{2013}" | "\u{2014}" => "-",
            word => word,
        })
        .collect::<Vec<_>>()
        .join(" ");
    let out = out
        .trim()
        .trim_end_matches(" - Topic")
        .trim_end_matches("VEVO");
    let words: Vec<&str> = trim_video_tags(out).split_whitespace().collect();
    // A removed tag leaves the dash that stood beside it: "Song - (Official
    // Video)", "[MV] - Artist - Song".
    let dash = |word: &&str| matches!(*word, "-" | "\u{2013}" | "\u{2014}");
    let start = words.iter().position(|word| !dash(word));
    let end = words.iter().rposition(|word| !dash(word));
    match (start, end) {
        (Some(start), Some(end)) => words[start..=end].join(" "),
        _ => String::new(),
    }
}

/// `title` without the track number of an album upload ("06 - ", "06. ",
/// "6) "). It comes off only when an "Artist - Song" split follows, so a song
/// named by a number ("22 - Taylor Swift") keeps it; a name with three or more
/// digits ("112 - Cupid") or no separator after the number ("50 Cent - In Da
/// Club") is never a track number. Arabic-Indic and Persian digits count too,
/// and so does an en or em dash after the number ("06– ").
fn strip_track_number(title: &str) -> &str {
    let is_digit = |c: char| {
        c.is_ascii_digit()
            || ('\u{660}'..='\u{669}').contains(&c)
            || ('\u{6F0}'..='\u{6F9}').contains(&c)
    };
    let digits = title.chars().take_while(|&c| is_digit(c)).count();
    if !(1..=2).contains(&digits) {
        return title;
    }
    let end: usize = title.chars().take(digits).map(char::len_utf8).sum();
    let Some(rest) = title[end..]
        .trim_start_matches(' ')
        .strip_prefix(['-', '.', ')', '\u{2013}', '\u{2014}'])
        .and_then(|rest| rest.strip_prefix(' '))
    else {
        return title;
    };
    let rest = rest.trim_start();
    if rest.contains(" - ") { rest } else { title }
}

/// A video title's song part: decoration removed, then any track number.
fn strip_title(text: &str) -> String {
    strip_track_number(&strip_decoration(text)).to_owned()
}

/// Strips video-title decoration and splits `Artist - Song` titles.
/// Returns `None` when there is nothing to clean.
fn clean_query(query: &Query) -> Option<Query> {
    let strip = strip_decoration;
    let title = strip_title(&query.title);
    // Only a channel name (empty, `|`, VEVO) or an artist that matches the left
    // part is replaced; otherwise "Song - Radio Edit" would become artist "Song".
    // A "- Topic" channel is not one of them: YouTube makes it for one artist and
    // titles its videos with the song alone, versions included ("Song -
    // Remastered 2011"), so the channel, cleaned, is the artist.
    let reported = query.artist.trim();
    let channel = reported.is_empty() || reported.contains('|') || reported.ends_with("VEVO");
    let (artist, title) = match title.split_once(" - ") {
        Some((left, song))
            if !song.trim().is_empty() && (channel || same_name(left, &strip(reported))) =>
        {
            (left.trim().to_owned(), song.trim().to_owned())
        }
        _ => (strip(&query.artist), title),
    };
    let cleaned = Query {
        title,
        artist,
        album: String::new(),
        duration_secs: query.duration_secs,
    };
    let unchanged = cleaned.title == query.title.trim() && cleaned.artist == query.artist.trim();
    (!unchanged && !cleaned.title.is_empty()).then_some(cleaned)
}

/// Whether one name's words begin the other's: "The Weeknd" and "The Weeknd &
/// Daft Punk" match, "Eve" and "Evergreen" don't.
fn same_name(a: &str, b: &str) -> bool {
    let words = |text: &str| -> Vec<String> {
        text.split(|c: char| !c.is_alphanumeric())
            .filter(|word| !word.is_empty())
            .map(str::to_lowercase)
            .collect()
    };
    let (a, b) = (words(a), words(b));
    !a.is_empty() && !b.is_empty() && (a.starts_with(&b) || b.starts_with(&a))
}

/// Prefers synced lyrics whose duration matches the track; a search can return
/// live versions and remasters of different lengths.
/// Order of preference: synced with a matching duration, synced whose duration
/// is unknown on either side, any synced, then plain or instrumental.
/// With `strict`, only synced lyrics of the right length (or of unknown length)
/// count.
fn pick_best(records: Vec<Record>, duration_secs: Option<u32>, strict: bool) -> Lyrics {
    let mut unknown_duration = None;
    let mut synced_any = None;
    let mut fallback = Lyrics::NotFound;
    for record in records {
        let fit = match (record.duration, duration_secs) {
            (Some(a), Some(b)) => Some((a - f64::from(b)).abs() <= 3.0),
            _ => None,
        };
        let lyrics = record.into_lyrics();
        match lyrics {
            Lyrics::Synced(_) if fit == Some(true) => return lyrics,
            Lyrics::Synced(_) if fit.is_none() => {
                unknown_duration.get_or_insert(lyrics);
            }
            Lyrics::Synced(_) => {
                synced_any.get_or_insert(lyrics);
            }
            Lyrics::Plain | Lyrics::Instrumental if fallback == Lyrics::NotFound => {
                fallback = lyrics
            }
            _ => {}
        }
    }
    if strict {
        return unknown_duration.unwrap_or(Lyrics::NotFound);
    }
    unknown_duration.or(synced_any).unwrap_or(fallback)
}

fn escape(text: &str) -> Result<String> {
    Ok(Uri::EscapeComponent(&HSTRING::from(text))?.to_string())
}

/// What a 503 answer means for a request. Other busy answers (rate limited,
/// gateway errors) are always tried again a couple of times: LRCLIB often
/// serves the same request fine a moment later.
#[derive(Clone, Copy, PartialEq)]
enum Busy {
    /// Busy for a moment: try again like the others.
    Retry,
    /// `/api/get` answers 503 for many songs it has no exact match for (the
    /// search still finds them): the same as 404.
    MeansAbsent,
}

/// Whether an answer means "LRCLIB has no such track".
fn means_absent(status: i32, busy: Busy) -> bool {
    status == 404 || (busy == Busy::MeansAbsent && status == 503)
}

/// `Ok(None)` for 404, which LRCLIB uses for "no such track".
fn get(client: &HttpClient, url: &str, busy: Busy) -> Result<Option<String>> {
    let uri = Uri::CreateUri(&HSTRING::from(url))?;
    let deadline = Instant::now() + TOTAL_TIMEOUT;
    let mut pauses = BUSY_PAUSES.iter();
    let response = loop {
        let response = wait(
            client.GetWithOptionAsync(&uri, HttpCompletionOption::ResponseHeadersRead)?,
            TIMEOUT.min(time_left(deadline)?),
        )?;
        let status = response.StatusCode()?.0;
        let retry = is_busy(status) && !means_absent(status, busy);
        match pauses.next() {
            Some(&pause) if retry && time_left(deadline)? > pause => std::thread::sleep(pause),
            _ => break response,
        }
    };
    let status = response.StatusCode()?;
    if means_absent(status.0, busy) {
        return Ok(None);
    }
    response.EnsureSuccessStatusCode()?;
    let content = response.Content()?;
    if let Ok(length) = content
        .Headers()?
        .ContentLength()
        .and_then(|length| length.Value())
        && length > MAX_BODY_BYTES
    {
        return Err(windows::core::Error::empty());
    }
    // Read in chunks so a response with no length header stops at the cap
    // instead of being buffered whole first.
    let stream = wait(content.ReadAsInputStreamAsync()?, time_left(deadline)?)?;
    let mut body = Vec::new();
    loop {
        let buffer = Buffer::Create(CHUNK_BYTES)?;
        let chunk = wait(
            stream.ReadAsync(&buffer, CHUNK_BYTES, InputStreamOptions::Partial)?,
            time_left(deadline)?,
        )?;
        let len = chunk.Length()? as usize;
        if len == 0 {
            break;
        }
        if (body.len() + len) as u64 > MAX_BODY_BYTES {
            return Err(windows::core::Error::empty());
        }
        let start = body.len();
        body.resize(start + len, 0);
        DataReader::FromBuffer(&chunk)?.ReadBytes(&mut body[start..])?;
    }
    Ok(Some(String::from_utf8_lossy(&body).into_owned()))
}

/// Time left until `deadline`, or a timeout error once it has passed.
fn time_left(deadline: Instant) -> Result<Duration> {
    remaining(deadline, Instant::now())
        .ok_or_else(|| windows::Win32::Foundation::ERROR_TIMEOUT.into())
}

fn remaining(deadline: Instant, now: Instant) -> Option<Duration> {
    deadline
        .checked_duration_since(now)
        .filter(|left| !left.is_zero())
}

/// Waits for a WinRT operation, cancelling it after `timeout`.
fn wait<T, P>(operation: IAsyncOperationWithProgress<T, P>, timeout: Duration) -> Result<T>
where
    T: RuntimeType + 'static,
    P: RuntimeType + 'static,
{
    let (done_tx, done_rx) = mpsc::channel();
    operation.SetCompleted(&AsyncOperationWithProgressCompletedHandler::new(
        move |_, _| {
            let _ = done_tx.send(());
            Ok(())
        },
    ))?;
    if done_rx.recv_timeout(timeout).is_err() {
        let _ = operation.Cancel();
        return Err(windows::Win32::Foundation::ERROR_TIMEOUT.into());
    }
    operation.GetResults()
}

// Bounds on what one lyrics response can make BUKU allocate. A line with many
// time tags is copied once per tag, so without them a hostile 512 KB response
// could expand to gigabytes. Real songs stay far below all three.
const MAX_TAGS_PER_LINE: usize = 16;
const MAX_LINE_CHARS: usize = 300;
const MAX_LINES: usize = 2_000;

/// Parses LRC text: `[mm:ss.xx] words`, with up to `MAX_TAGS_PER_LINE` time tags per line.
pub fn parse_lrc(text: &str) -> Vec<Line> {
    let mut lines = Vec::new();
    for raw in text.lines() {
        let mut rest = raw.trim();
        let mut times = Vec::new();
        while let Some(tag) = rest.strip_prefix('[') {
            let Some(close) = tag.find(']') else { break };
            match parse_time(&tag[..close]) {
                Some(ms) if times.len() < MAX_TAGS_PER_LINE => times.push(ms),
                Some(_) => {}
                // Metadata tags like [ar:...] carry no time; the line is skipped.
                None => break,
            }
            rest = &tag[close + 1..];
        }
        let words: String = rest.trim().chars().take(MAX_LINE_CHARS).collect();
        for time_ms in times {
            if lines.len() == MAX_LINES {
                break;
            }
            lines.push(Line {
                time_ms,
                text: words.clone(),
            });
        }
    }
    lines.sort_by_key(|line| line.time_ms);
    lines
}

/// `mm:ss` or `mm:ss.xx`, digits only. Anything else (exponents, signs, a minute
/// count too large to hold in milliseconds) is not a time tag.
fn parse_time(tag: &str) -> Option<u64> {
    let digits = |text: &str| !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit());
    let (minutes, seconds) = tag.split_once(':')?;
    let (minutes, seconds) = (minutes.trim(), seconds.trim());
    let (whole, fraction) = seconds.split_once('.').unwrap_or((seconds, ""));
    if !digits(minutes) || !digits(whole) || !(fraction.is_empty() || digits(fraction)) {
        return None;
    }
    let minutes: u64 = minutes.parse().ok()?;
    let seconds: f64 = seconds.parse().ok()?;
    if !(0.0..60.0).contains(&seconds) {
        return None;
    }
    minutes
        .checked_mul(60_000)?
        .checked_add((seconds * 1000.0).round() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_lyrics_client_should_keep_nothing_on_disk() {
        let settings = private_filter().and_then(|filter| {
            let cache = filter.CacheControl()?;
            Ok((
                cache.ReadBehavior()?,
                cache.WriteBehavior()?,
                filter.CookieUsageBehavior()?,
                filter.AllowAutoRedirect()?,
                filter.AllowUI()?,
            ))
        });
        assert_eq!(
            settings.ok(),
            Some((
                HttpCacheReadBehavior::NoCache,
                HttpCacheWriteBehavior::NoCache,
                HttpCookieUsageBehavior::NoCookies,
                false,
                false,
            ))
        );
    }

    #[test]
    fn a_body_that_is_not_json_should_be_an_error_not_no_lyrics() {
        let parse = |body: Option<&str>| read_json::<Vec<Record>>(body.map(str::to_owned));
        assert!(parse(Some("<html>Maintenance</html>")).is_err());
        assert!(matches!(parse(None), Ok(None)));
        assert!(matches!(parse(Some("[]")), Ok(Some(records)) if records.is_empty()));
    }

    #[test]
    fn parse_lrc_should_read_each_timed_line() {
        let lines = parse_lrc("[ar:Someone]\n[00:01.50] Hello\n[01:02.00]World\n");
        assert_eq!(
            lines,
            vec![
                Line {
                    time_ms: 1500,
                    text: "Hello".into()
                },
                Line {
                    time_ms: 62_000,
                    text: "World".into()
                },
            ]
        );
    }

    #[test]
    fn parse_lrc_should_expand_repeated_time_tags() {
        let lines = parse_lrc("[00:10.00][00:20.00]Chorus");
        assert_eq!(
            lines.iter().map(|line| line.time_ms).collect::<Vec<_>>(),
            vec![10_000, 20_000]
        );
    }

    #[test]
    fn parse_lrc_should_bound_output_for_hostile_input() {
        let line = format!("{}{}", "[00:01.00]".repeat(10_000), "x".repeat(100_000));
        let text = vec![line; 50].join("\n");
        let lines = parse_lrc(&text);
        let bytes: usize = lines.iter().map(|line| line.text.len()).sum();
        assert!(lines.len() <= MAX_LINES && bytes <= MAX_LINES * MAX_LINE_CHARS);
    }

    #[test]
    fn split_guesses_should_try_both_halves_as_the_artist() {
        let query = Query {
            title: "ابتعد عني - ذكرى (Official Video)".into(),
            artist: "على وتر القلب".into(),
            album: String::new(),
            duration_secs: Some(364),
        };
        let pairs: Vec<(String, String)> = split_guesses(&query)
            .into_iter()
            .map(|guess| (guess.artist, guess.title))
            .collect();
        assert_eq!(
            pairs,
            [
                ("ابتعد عني".to_owned(), "ذكرى".to_owned()),
                ("ذكرى".to_owned(), "ابتعد عني".to_owned()),
            ]
        );
    }

    #[test]
    fn video_tags_should_come_off_the_end_of_a_title() {
        let titles = [
            "Hit 'Em Up HD",
            "Song - Official Music Video",
            "Song Lyrics 4K",
            "HD",
            "Audio",
            "Radio",
            "ابتعد عني HD",
            // A Kelvin sign (three bytes) that lowercases to an ASCII "k".
            "Song 4\u{212A}",
        ]
        .map(trim_video_tags);
        assert_eq!(
            titles,
            [
                "Hit 'Em Up",
                "Song",
                "Song",
                "HD",
                "Audio",
                "Radio",
                "ابتعد عني",
                "Song 4\u{212A}"
            ]
        );
    }

    #[test]
    fn a_video_title_with_tags_should_give_clean_guesses() {
        let query = Query {
            title: "2Pac - Hit 'Em Up (Dirty) (Music Video) HD".into(),
            artist: "Seven Hip-Hop".into(),
            album: String::new(),
            duration_secs: Some(323),
        };
        let first = split_guesses(&query)
            .into_iter()
            .next()
            .map(|guess| (guess.artist, guess.title));
        assert_eq!(first, Some(("2Pac".to_owned(), "Hit 'Em Up".to_owned())));
    }

    #[test]
    fn split_guesses_should_not_guess_without_a_duration() {
        let query = Query {
            title: "A - B".into(),
            artist: "x".into(),
            album: String::new(),
            duration_secs: None,
        };
        assert!(split_guesses(&query).is_empty());
    }

    #[test]
    fn a_busy_answer_should_mean_absent_only_for_the_exact_lookup() {
        let answers = [
            means_absent(404, Busy::Retry),
            means_absent(503, Busy::MeansAbsent),
            means_absent(503, Busy::Retry),
            means_absent(500, Busy::MeansAbsent),
        ];
        assert_eq!(answers, [true, true, false, false]);
    }

    #[test]
    fn split_guesses_should_leave_titles_without_one_clear_split() {
        let titles = ["Song", "A - B - C", " - Song"];
        let counts = titles.map(|title| {
            split_guesses(&Query {
                title: title.into(),
                artist: "x".into(),
                album: String::new(),
                duration_secs: Some(200),
            })
            .len()
        });
        assert_eq!(counts, [0, 0, 0]);
    }

    #[test]
    fn clean_query_should_split_video_titles() {
        let query = Query {
            title: "أحلام - حزين (ألبوم فدوة عيونك) | 2018".into(),
            artist: "Ahlam | أحلام".into(),
            album: String::new(),
            duration_secs: Some(326),
        };
        let cleaned = clean_query(&query).map(|q| (q.artist, q.title));
        assert_eq!(cleaned, Some(("أحلام".to_owned(), "حزين".to_owned())));
    }

    #[test]
    fn clean_query_should_keep_the_artist_for_version_suffixes() {
        let query = Query {
            title: "Wake Me Up - Radio Edit".into(),
            artist: "Avicii".into(),
            album: String::new(),
            duration_secs: Some(247),
        };
        assert_eq!(clean_query(&query), None);
    }

    #[test]
    fn clean_query_should_leave_clean_titles_alone() {
        let query = Query {
            title: "Blinding Lights".into(),
            artist: "The Weeknd".into(),
            album: "After Hours".into(),
            duration_secs: Some(200),
        };
        assert_eq!(clean_query(&query), None);
    }

    #[test]
    fn remaining_should_count_down_to_the_deadline() {
        let now = Instant::now();
        let deadline = now + Duration::from_secs(5);
        assert_eq!(remaining(deadline, now), Some(Duration::from_secs(5)));
        assert_eq!(
            remaining(deadline, now + Duration::from_secs(2)),
            Some(Duration::from_secs(3))
        );
    }

    #[test]
    fn remaining_should_be_none_once_the_deadline_has_passed() {
        let now = Instant::now();
        let deadline = now + Duration::from_secs(5);
        assert_eq!(remaining(deadline, deadline), None);
        assert_eq!(remaining(deadline, deadline + Duration::from_secs(1)), None);
    }

    #[test]
    fn pick_best_should_prefer_matching_duration() {
        let record = |duration: f64, text: &str| Record {
            instrumental: false,
            plain_lyrics: None,
            synced_lyrics: Some(format!("[00:01.00]{text}")),
            duration: Some(duration),
        };
        let picked = pick_best(
            vec![record(300.0, "live"), record(200.0, "studio")],
            Some(201),
            false,
        );
        assert_eq!(
            picked,
            Lyrics::Synced(vec![Line {
                time_ms: 1000,
                text: "studio".into()
            }])
        );
    }
}

#[cfg(test)]
mod network_tests {
    use super::*;

    /// Talks to lrclib.net; run with `cargo test -- --ignored`.
    #[test]
    #[ignore = "needs network access"]
    fn fetch_should_find_synced_lyrics_for_a_known_song() {
        let query = Query {
            title: "Blinding Lights".into(),
            artist: "The Weeknd".into(),
            album: "After Hours".into(),
            duration_secs: Some(200),
        };
        assert!(matches!(fetch(&query), Ok(Lyrics::Synced(lines)) if lines.len() > 10));
    }

    /// An album upload: track number first, a channel that isn't the artist.
    #[test]
    #[ignore = "needs network access"]
    fn fetch_should_find_lyrics_for_an_album_upload() {
        let query = Query {
            title: "06 - Ice Cube - Bow Down (Westside Connection)".into(),
            artist: "O'Shea Jackson".into(),
            album: String::new(),
            duration_secs: Some(210),
        };
        assert!(matches!(fetch(&query), Ok(Lyrics::Synced(lines)) if lines.len() > 10));
    }
}

#[cfg(test)]
mod edge_case_tests {
    use super::*;

    #[test]
    fn only_busy_answers_should_be_retried() {
        let codes = [429, 502, 503, 504, 200, 404, 500];
        assert_eq!(
            codes.map(is_busy),
            [true, true, true, true, false, false, false]
        );
    }

    fn synced(duration: Option<f64>, text: &str) -> Record {
        Record {
            instrumental: false,
            plain_lyrics: None,
            synced_lyrics: Some(format!("[00:01.00]{text}")),
            duration,
        }
    }

    fn text_of(lyrics: &Lyrics) -> Option<&str> {
        match lyrics {
            Lyrics::Synced(lines) => lines.first().map(|line| line.text.as_str()),
            _ => None,
        }
    }

    #[test]
    fn a_time_tag_too_large_to_hold_should_be_rejected() {
        assert_eq!(parse_time("307445734561826:00.00"), None);
    }

    #[test]
    fn a_time_tag_in_exponent_notation_should_be_rejected() {
        assert_eq!(parse_time("00:1e1"), None);
    }

    #[test]
    fn an_artist_that_only_starts_a_longer_word_should_not_swap_title_and_artist() {
        let query = Query {
            title: "Evergreen - Remastered".into(),
            artist: "Eve".into(),
            album: String::new(),
            duration_secs: Some(200),
        };
        assert_eq!(clean_query(&query), None);
    }

    #[test]
    fn a_featured_artist_line_should_still_split() {
        let query = Query {
            title: "The Weeknd & Daft Punk - Starboy".into(),
            artist: "The Weeknd".into(),
            album: String::new(),
            duration_secs: Some(230),
        };
        let cleaned = clean_query(&query).map(|q| (q.artist, q.title));
        assert_eq!(
            cleaned,
            Some(("The Weeknd & Daft Punk".into(), "Starboy".into()))
        );
    }

    #[test]
    fn a_matching_duration_should_beat_an_unknown_one() {
        let picked = pick_best(
            vec![synced(None, "unknown"), synced(Some(200.0), "right")],
            Some(200),
            false,
        );
        assert_eq!(text_of(&picked), Some("right"));
    }
}

#[cfg(test)]
mod lookup_tests {
    use super::*;

    fn upload(title: &str, channel: &str, duration_secs: u32) -> Query {
        Query {
            title: title.into(),
            artist: channel.into(),
            album: String::new(),
            duration_secs: Some(duration_secs),
        }
    }

    fn first_guess(query: &Query) -> Option<(String, String)> {
        split_guesses(query)
            .into_iter()
            .next()
            .map(|guess| (guess.artist, guess.title))
    }

    #[test]
    fn a_track_number_should_come_off_an_album_upload_title() {
        let titles = [
            "06 - Ice Cube - Bow Down",
            "06. Ice Cube - Bow Down",
            "6) Ice Cube - Bow Down",
            "6 -  Ice Cube - Bow Down",
        ]
        .map(strip_track_number);
        assert_eq!(titles, ["Ice Cube - Bow Down"; 4]);
    }

    #[test]
    fn a_number_that_is_part_of_the_name_should_stay() {
        let titles = [
            "22 - Taylor Swift",
            "112 - Cupid",
            "50 Cent - In Da Club",
            "21 Savage - a lot",
            "2Pac - Changes",
            "1979",
            "06-Ice Cube - Bow Down",
        ];
        assert_eq!(titles.map(strip_track_number), titles);
    }

    #[test]
    fn an_album_upload_should_guess_artist_and_song() {
        let query = upload(
            "06 - Ice Cube - Bow Down (Westside Connection)",
            "O'Shea Jackson",
            210,
        );
        assert_eq!(
            first_guess(&query),
            Some(("Ice Cube".to_owned(), "Bow Down".to_owned()))
        );
    }

    #[test]
    fn a_long_dash_should_split_like_a_hyphen() {
        let expected = Some(("Ice Cube".to_owned(), "Bow Down".to_owned()));
        for title in [
            "Ice Cube \u{2013} Bow Down",
            "Ice Cube \u{2014} Bow Down (Official Video)",
            "Ice Cube \u{2013} Bow Down \u{2013} Official Video",
            "06 \u{2013} Ice Cube \u{2013} Bow Down",
        ] {
            assert_eq!(
                first_guess(&upload(title, "Someone", 210)),
                expected,
                "{title}"
            );
        }
    }

    #[test]
    fn only_a_spaced_dash_should_become_a_separator() {
        // Only a spaced dash separates; "Jay-Z" and "Spider-Man" stay whole.
        assert_eq!(
            strip_decoration("Jay-Z \u{2013} 99 Problems"),
            "Jay-Z - 99 Problems"
        );
        assert_eq!(
            strip_decoration("Spider-Man\u{2013}Theme"),
            "Spider-Man\u{2013}Theme"
        );
    }

    #[test]
    fn a_topic_channel_title_should_stay_the_song() {
        // YouTube's "Artist - Topic" channels title a video with the song alone,
        // version included; the channel names the artist.
        let cleaned = clean_query(&upload(
            "Bohemian Rhapsody - Remastered 2011",
            "Queen - Topic",
            355,
        ));
        assert_eq!(
            cleaned.map(|query| (query.artist, query.title)),
            Some((
                "Queen".to_owned(),
                "Bohemian Rhapsody - Remastered 2011".to_owned()
            ))
        );
    }

    #[test]
    fn a_three_digit_name_should_not_be_a_track_number() {
        assert_eq!(
            strip_track_number("112 - Cupid - Live"),
            "112 - Cupid - Live"
        );
    }

    #[test]
    fn nested_brackets_should_come_off_whole() {
        assert_eq!(
            strip_decoration("Adele - Hello (Live (2016) Remastered)"),
            "Adele - Hello"
        );
    }

    #[test]
    fn a_tag_glued_to_a_word_should_stay() {
        // "Shahd" ends in "hd", but nothing separates it, so it is a name.
        assert_eq!(strip_decoration("Shahd"), "Shahd");
    }

    #[test]
    fn a_tag_in_full_width_brackets_should_come_off() {
        // Japanese and Chinese titles write brackets full width.
        assert_eq!(
            [
                "YOASOBI - \u{30a2}\u{30a4}\u{30c9}\u{30eb}\u{ff08}Official Music Video\u{ff09}",
                "YOASOBI - \u{30a2}\u{30a4}\u{30c9}\u{30eb}\u{ff3b}MV\u{ff3d}",
            ]
            .map(strip_decoration),
            ["YOASOBI - \u{30a2}\u{30a4}\u{30c9}\u{30eb}"; 2]
        );
    }

    #[test]
    fn a_dash_left_beside_a_removed_tag_should_go_with_it() {
        let expected = Some(("Ice Cube".to_owned(), "Bow Down".to_owned()));
        for title in [
            "Ice Cube - Bow Down - (Official Video)",
            "Ice Cube - Bow Down - [Official Video]",
            "[Official MV] - Ice Cube - Bow Down",
            "(Official) \u{2013} Ice Cube \u{2013} Bow Down",
        ] {
            assert_eq!(
                first_guess(&upload(title, "Someone", 210)),
                expected,
                "{title}"
            );
        }
    }

    #[test]
    fn a_dash_between_no_break_spaces_should_split_like_a_hyphen() {
        assert_eq!(
            first_guess(&upload(
                "Ice Cube\u{a0}\u{2013}\u{a0}Bow Down",
                "Someone",
                210
            )),
            Some(("Ice Cube".to_owned(), "Bow Down".to_owned()))
        );
    }

    #[test]
    fn a_track_number_should_come_off_in_every_form_uploads_use() {
        let titles = [
            "06\u{2013} Ice Cube - Bow Down",
            "06 \u{2014} Ice Cube - Bow Down",
            "\u{660}\u{666} - Ice Cube - Bow Down",
            "\u{6F0}\u{6F6}. Ice Cube - Bow Down",
        ]
        .map(strip_title);
        assert_eq!(titles, ["Ice Cube - Bow Down"; 4]);
    }

    fn synced_record(duration: Option<f64>) -> Record {
        Record {
            instrumental: false,
            plain_lyrics: None,
            synced_lyrics: Some("[00:01.00]line".into()),
            duration,
        }
    }

    #[test]
    fn a_guess_should_only_take_lyrics_of_the_right_length() {
        let picked = [
            pick_best(vec![synced_record(Some(100.0))], Some(200), true),
            pick_best(vec![synced_record(Some(201.0))], Some(200), true),
            pick_best(vec![synced_record(None)], Some(200), true),
            pick_best(vec![synced_record(Some(100.0))], Some(200), false),
        ]
        .map(|lyrics| lyrics != Lyrics::NotFound);
        assert_eq!(picked, [false, true, true, true]);
    }
}

#[cfg(test)]
mod prop_tests {
    use super::*;
    use proptest::prelude::*;

    /// Words as a lyric line carries them: no brackets, newlines or edge spaces.
    fn words() -> impl Strategy<Value = String> {
        proptest::string::string_regex("[a-zA-Z\u{600}-\u{6ff}'!,.? ]{1,40}")
            .expect("pattern")
            .prop_map(|text| text.trim().to_owned())
            .prop_filter("words", |text| !text.is_empty())
    }

    proptest! {
        #[test]
        fn a_written_lrc_should_read_back(lines in prop::collection::vec((0u64..6_000_000, words()), 0..60)) {
            let text: String = lines
                .iter()
                .map(|(ms, words)| format!("[{:02}:{:02}.{:02}]{words}\n", ms / 60_000, ms / 1000 % 60, ms % 1000 / 10))
                .collect();
            let mut expected: Vec<(u64, String)> =
                lines.iter().map(|(ms, words)| (ms / 10 * 10, words.clone())).collect();
            expected.sort_by_key(|(ms, _)| *ms);
            let got: Vec<(u64, String)> = parse_lrc(&text).into_iter().map(|l| (l.time_ms, l.text)).collect();
            // Stable sort on both sides keeps lines that share a time in file order.
            prop_assert_eq!(got, expected);
        }

        #[test]
        fn any_text_should_parse_within_the_caps(text in r"(\[[0-9:.a-z]{0,8}\]|[^\[]{0,20}|\n){0,200}") {
            let lines = parse_lrc(&text);
            prop_assert!(lines.len() <= MAX_LINES);
            prop_assert!(lines.windows(2).all(|pair| pair[0].time_ms <= pair[1].time_ms));
            prop_assert!(lines.iter().all(|line| line.text.chars().count() <= MAX_LINE_CHARS));
        }

        #[test]
        fn cleaning_a_title_twice_should_change_nothing(title in "[a-zA-Z0-9 \u{2013}\u{a0}()|.\\[\\]-]{0,40}") {
            let once = strip_decoration(&title);
            prop_assert_eq!(strip_decoration(&once), once.clone());
            prop_assert!(!once.starts_with("- ") && !once.ends_with(" -") && once != "-", "{:?}", once);
        }
    }
}
