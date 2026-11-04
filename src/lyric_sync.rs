//! Per-song lyric timing set on the Music card, stored in
//! `%APPDATA%\BUKU\lyric_sync.json`.
//!
//! Synced lyrics can belong to another cut of a song: a music video often has an
//! intro the album version lacks, so every line lands early by the same amount.
//! The user lines the lyrics up once and the offset is kept for that song.
//! Songs are stored by a hash of title, artist and length, never by name.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

use crate::lyrics::Line;
use crate::store;

const FILE: &str = "lyric_sync.json";
/// Further than any intro or outro a song version adds.
pub const MAX_OFFSET_MS: i64 = 10 * 60 * 1000;
/// Songs remembered; the oldest setting is dropped first.
const MAX_SONGS: usize = 500;

/// How the user lined the lyrics up, each relative to the moment of the click.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// The line shown starts now.
    Shown,
    /// The line before the one shown starts now: the lyrics were ahead.
    Previous,
    /// The line after the one shown starts now: the lyrics were behind.
    Next,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Entry {
    song: u64,
    /// Positive when the lyrics show later than their file says.
    offset_ms: i64,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct LyricSync {
    /// The most recently set song last.
    songs: Vec<Entry>,
}

/// The key a song is stored under: FNV-1a over title, artist and length in whole
/// seconds. The length keeps a video's timing off the album cut of the same song.
pub fn song_key(title: &str, artist: &str, duration_ms: u64) -> u64 {
    const PRIME: u64 = 0x0100_0000_01b3;
    let seconds = duration_ms.saturating_add(500) / 1000;
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    // 0xFF never occurs in UTF-8, so "ab"+"c" and "a"+"bc" differ.
    let bytes = title
        .bytes()
        .chain([0xFF])
        .chain(artist.bytes())
        .chain([0xFF])
        .chain(seconds.to_le_bytes());
    for byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

/// The position to look lyrics up at, for a playback position and an offset;
/// negative while playback hasn't yet reached where the shifted lyrics begin.
pub fn lyric_position(position_ms: u64, offset_ms: i64) -> i64 {
    i64::try_from(position_ms)
        .unwrap_or(i64::MAX)
        .saturating_sub(offset_ms)
}

/// The offset that makes a line timed at `line_ms` start at `position_ms`.
pub fn offset_starting(line_ms: u64, position_ms: u64) -> i64 {
    let difference = i128::from(position_ms) - i128::from(line_ms);
    difference.clamp(-i128::from(MAX_OFFSET_MS), i128::from(MAX_OFFSET_MS)) as i64
}

/// The file time of the line `step` asks for, given the line shown now
/// (`shown`, an index into the sorted `lines`). Lines sharing a time show
/// together, so a step always moves to a different time.
pub fn target_time(lines: &[Line], shown: Option<usize>, step: Step) -> Option<u64> {
    let first = lines.first()?.time_ms;
    let Some(current) = shown
        .and_then(|index| lines.get(index))
        .map(|line| line.time_ms)
    else {
        // Before the first line only "next" means anything.
        return (step == Step::Next).then_some(first);
    };
    match step {
        // A blank line marks a break: there is nothing to hear start.
        Step::Shown => shown
            .and_then(|index| lines.get(index))
            .filter(|line| !line.text.trim().is_empty())
            .map(|_| current),
        Step::Previous => lines
            .iter()
            .map(|line| line.time_ms)
            .filter(|&time| time < current)
            .max(),
        Step::Next => lines
            .iter()
            .map(|line| line.time_ms)
            .filter(|&time| time > current)
            .min(),
    }
}

/// How the card shows an offset: "Lyrics +19.1 s".
pub fn describe(offset_ms: i64) -> String {
    let tenths = (offset_ms as f64 / 100.0).round() as i64;
    let sign = if tenths < 0 { '-' } else { '+' };
    let tenths = tenths.unsigned_abs();
    format!("Lyrics {sign}{}.{} s", tenths / 10, tenths % 10)
}

impl LyricSync {
    pub fn load() -> Self {
        store::load(FILE)
    }

    pub fn offset_ms(&self, song: u64) -> i64 {
        self.songs
            .iter()
            .rev()
            .find(|entry| entry.song == song)
            .map_or(0, |entry| entry.offset_ms)
    }

    /// Sets a song's offset and saves; 0 forgets the song.
    pub fn set(&mut self, song: u64, offset_ms: i64) {
        self.remember(song, offset_ms);
        // Losing a timing to a full disk isn't worth interrupting anyone over.
        let _ = store::save(FILE, self);
    }

    fn remember(&mut self, song: u64, offset_ms: i64) {
        self.songs.retain(|entry| entry.song != song);
        let offset_ms = offset_ms.clamp(-MAX_OFFSET_MS, MAX_OFFSET_MS);
        // Under what the label shows (a tenth of a second, rounded) is no offset.
        if offset_ms.abs() >= 50 {
            self.songs.push(Entry { song, offset_ms });
        }
        let extra = self.songs.len().saturating_sub(MAX_SONGS);
        self.songs.drain(..extra);
    }
}

impl Serialize for LyricSync {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Stored {
            song: String,
            offset_ms: i64,
        }
        #[derive(Serialize)]
        struct File {
            songs: Vec<Stored>,
        }
        File {
            songs: self
                .songs
                .iter()
                .map(|entry| Stored {
                    song: format!("{:016x}", entry.song),
                    offset_ms: entry.offset_ms,
                })
                .collect(),
        }
        .serialize(serializer)
    }
}

/// Read entry by entry, like the other files: one unusable entry doesn't throw
/// away the rest.
impl<'de> Deserialize<'de> for LyricSync {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        let mut sync = Self::default();
        let entries = value.get("songs").and_then(Value::as_array);
        for entry in entries.into_iter().flatten() {
            let song = entry
                .get("song")
                .and_then(Value::as_str)
                .filter(|hex| hex.len() == 16)
                .and_then(|hex| u64::from_str_radix(hex, 16).ok());
            let offset_ms = entry.get("offset_ms").and_then(Value::as_i64);
            if let (Some(song), Some(offset_ms)) = (song, offset_ms) {
                sync.remember(song, offset_ms);
            }
        }
        Ok(sync)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(times: &[u64]) -> Vec<Line> {
        times
            .iter()
            .map(|&time_ms| Line {
                time_ms,
                text: format!("at {time_ms}"),
            })
            .collect()
    }

    #[test]
    fn a_positive_offset_should_show_lyrics_later() {
        assert_eq!(lyric_position(30_000, 19_000), 11_000);
    }

    #[test]
    fn a_negative_offset_should_show_lyrics_earlier() {
        assert_eq!(lyric_position(30_000, -2_000), 32_000);
    }

    #[test]
    fn a_position_inside_the_offset_should_be_before_the_lyrics() {
        assert_eq!(lyric_position(5_000, 19_000), -14_000);
    }

    #[test]
    fn the_offset_for_a_line_should_put_it_at_the_position() {
        let offset = offset_starting(500, 19_600);
        assert_eq!((offset, lyric_position(19_600, offset)), (19_100, 500));
    }

    #[test]
    fn an_offset_should_be_capped() {
        assert_eq!(offset_starting(0, u64::MAX), MAX_OFFSET_MS);
        assert_eq!(offset_starting(u64::MAX, 0), -MAX_OFFSET_MS);
    }

    #[test]
    fn this_line_should_be_the_line_shown() {
        let lines = lines(&[1_000, 4_000, 7_000]);
        assert_eq!(target_time(&lines, Some(1), Step::Shown), Some(4_000));
    }

    #[test]
    fn previous_and_next_should_move_one_line() {
        let lines = lines(&[1_000, 4_000, 7_000]);
        assert_eq!(
            [
                target_time(&lines, Some(1), Step::Previous),
                target_time(&lines, Some(1), Step::Next),
            ],
            [Some(1_000), Some(7_000)]
        );
    }

    #[test]
    fn steps_should_stop_at_either_end() {
        let lines = lines(&[1_000, 4_000]);
        assert_eq!(
            [
                target_time(&lines, Some(0), Step::Previous),
                target_time(&lines, Some(1), Step::Next),
            ],
            [None, None]
        );
    }

    #[test]
    fn before_the_first_line_only_next_should_move() {
        let lines = lines(&[1_000, 4_000]);
        assert_eq!(
            [
                target_time(&lines, None, Step::Shown),
                target_time(&lines, None, Step::Previous),
                target_time(&lines, None, Step::Next),
            ],
            [None, None, Some(1_000)]
        );
    }

    #[test]
    fn a_step_should_skip_lines_sharing_a_time() {
        let lines = lines(&[1_000, 4_000, 4_000, 7_000]);
        assert_eq!(
            [
                target_time(&lines, Some(2), Step::Previous),
                target_time(&lines, Some(1), Step::Next),
            ],
            [Some(1_000), Some(7_000)]
        );
    }

    #[test]
    fn no_lines_should_give_no_target() {
        assert_eq!(target_time(&[], Some(0), Step::Next), None);
    }

    #[test]
    fn an_offset_should_read_with_its_sign() {
        assert_eq!(
            [describe(19_120), describe(-500), describe(0)],
            ["Lyrics +19.1 s", "Lyrics -0.5 s", "Lyrics +0.0 s"]
        );
    }

    #[test]
    fn song_keys_should_tell_title_and_artist_apart() {
        assert_ne!(song_key("ab", "c", 0), song_key("a", "bc", 0));
        assert_eq!(
            song_key("Song", "Artist", 200_000),
            song_key("Song", "Artist", 200_000)
        );
    }

    #[test]
    fn a_cut_of_another_length_should_keep_its_own_timing() {
        // The music video runs 272 s, the album version 226 s.
        assert_ne!(
            song_key("Surround Sound", "J.I.D", 271_841),
            song_key("Surround Sound", "J.I.D", 226_000)
        );
    }

    #[test]
    fn a_length_reported_a_little_differently_should_be_the_same_song() {
        assert_eq!(
            song_key("Song", "Artist", 271_600),
            song_key("Song", "Artist", 272_400)
        );
    }

    #[test]
    fn a_blank_line_should_not_be_lined_up() {
        let mut lines = lines(&[1_000, 4_000, 7_000]);
        lines[1].text = "  ".into();
        assert_eq!(
            [
                target_time(&lines, Some(1), Step::Shown),
                target_time(&lines, Some(1), Step::Next),
            ],
            [None, Some(7_000)]
        );
    }

    #[test]
    fn an_offset_too_small_to_see_should_forget_the_song() {
        let mut sync = LyricSync::default();
        sync.remember(7, -40);
        assert_eq!(sync.offset_ms(7), 0);
    }

    #[test]
    fn a_rounded_zero_should_read_without_a_minus() {
        assert_eq!(
            [describe(-40), describe(-60)],
            ["Lyrics +0.0 s", "Lyrics -0.1 s"]
        );
    }

    #[test]
    fn setting_zero_should_forget_the_song() {
        let mut sync = LyricSync::default();
        sync.remember(7, 19_000);
        sync.remember(7, 0);
        assert_eq!((sync.offset_ms(7), sync.songs.len()), (0, 0));
    }

    #[test]
    fn the_oldest_song_should_be_dropped_first() {
        let mut sync = LyricSync::default();
        for song in 0..=MAX_SONGS as u64 {
            sync.remember(song, 1_000);
        }
        assert_eq!(
            (
                sync.songs.len(),
                sync.offset_ms(0),
                sync.offset_ms(MAX_SONGS as u64)
            ),
            (MAX_SONGS, 0, 1_000)
        );
    }

    #[test]
    fn a_saved_file_should_read_back_the_same() {
        let mut sync = LyricSync::default();
        sync.remember(u64::MAX, -2_500);
        sync.remember(42, 19_100);
        let json = serde_json::to_string(&sync).unwrap();
        assert_eq!(serde_json::from_str::<LyricSync>(&json).unwrap(), sync);
    }

    #[test]
    fn the_file_should_hold_no_song_names() {
        let mut sync = LyricSync::default();
        sync.remember(song_key("Surround Sound", "J.I.D", 271_841), 19_100);
        let json = serde_json::to_string(&sync).unwrap();
        assert!(
            !json.contains("Surround") && !json.contains("J.I.D"),
            "{json}"
        );
    }

    #[test]
    fn one_unusable_entry_should_not_lose_the_others() {
        let json = r#"{"songs":[
            {"song":"000000000000002a","offset_ms":19100},
            {"song":"not hex at all!!","offset_ms":5},
            {"song":"0000000000000007","offset_ms":"soon"},
            {"song":"0000000000000009","offset_ms":99999999999}
        ]}"#;
        let sync: LyricSync = serde_json::from_str(json).unwrap();
        assert_eq!(
            [sync.offset_ms(42), sync.offset_ms(7), sync.offset_ms(9)],
            [19_100, 0, MAX_OFFSET_MS]
        );
    }
}
