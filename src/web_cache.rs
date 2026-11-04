//! Lyrics lookups kept in the Windows web cache.
//!
//! Lookups bypass the cache, but a cached `lrclib.net` request would hold the
//! title it searched for in the user's WinINet cache. Clearing removes any such
//! entries and nothing else.

use std::collections::HashSet;

use windows::Win32::Foundation::{
    ERROR_FILE_NOT_FOUND, ERROR_INSUFFICIENT_BUFFER, ERROR_NO_MORE_ITEMS, HANDLE,
};
use windows::Win32::Networking::WinInet::{
    DeleteUrlCacheEntryW, FindCloseUrlCache, FindFirstUrlCacheEntryW, FindNextUrlCacheEntryW,
    INTERNET_CACHE_ENTRY_INFOW,
};
use windows::core::{HRESULT, HSTRING, PCWSTR};

/// The only host BUKU ever asked for lyrics.
const LYRICS_HOST: &str = "lrclib.net";

/// What a clear removed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Cleared {
    pub entries: u32,
    pub bytes: u64,
    /// Entries found but not removed.
    pub failed: u32,
    /// The cache couldn't be read to the end, so entries may remain unseen.
    pub scan_failed: bool,
}

/// Why an entry wasn't deleted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Gone {
    /// Removed in between: nothing left to do.
    Already,
    /// Still there: in use, or refused.
    Failed,
}

/// True for a cached `http(s)://lrclib.net/...` URL, and only that host.
pub fn is_lyrics_url(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    let Some(rest) = lower
        .strip_prefix("https://")
        .or_else(|| lower.strip_prefix("http://"))
    else {
        return false;
    };
    // The authority ends at the path, query or fragment; a backslash counts too,
    // as browsers read it as a slash.
    let authority = &rest[..rest.find(['/', '?', '#', '\\']).unwrap_or(rest.len())];
    // A user name comes before the last '@', a port after the ':'.
    let host = authority.rsplit('@').next().unwrap_or_default();
    let host = host.split(':').next().unwrap_or_default();
    host.strip_suffix('.').unwrap_or(host) == LYRICS_HOST
}

/// "4.2 KB" style size.
fn size(bytes: u64) -> String {
    const KB: u64 = 1024;
    match bytes {
        0..KB => format!("{bytes} B"),
        _ if bytes < KB * KB => format!("{:.1} KB", bytes as f64 / KB as f64),
        _ => format!("{:.1} MB", bytes as f64 / (KB * KB) as f64),
    }
}

/// The Settings window's two-line report of a clear.
pub fn describe(cleared: &Cleared) -> String {
    let plural = |n: u32| if n == 1 { "entry" } else { "entries" };
    match (cleared.entries, cleared.failed, cleared.scan_failed) {
        (0, 0, true) => "Couldn't read\nthe web cache".to_owned(),
        (0, 0, false) => "Nothing to clear".to_owned(),
        (0, failed, _) => format!("Couldn't clear\n{failed} {}", plural(failed)),
        (entries, 0, true) => format!("Cleared {entries} {}\ncache partly read", plural(entries)),
        (entries, 0, false) => format!(
            "Cleared {entries} {}\n{} freed",
            plural(entries),
            size(cleared.bytes)
        ),
        (entries, failed, _) => format!("Cleared {entries} {}\n{failed} in use", plural(entries)),
    }
}

const SCAN_FAILED_BIT: usize = 1 << 63;

/// A result packed into a window message: entries, failures and the scan flag
/// in `wParam`, bytes in `lParam`.
pub fn to_message(cleared: &Cleared) -> (usize, isize) {
    let failed = (cleared.failed as usize).min(u32::MAX as usize >> 1);
    let mut wparam = cleared.entries as usize | (failed << 32);
    if cleared.scan_failed {
        wparam |= SCAN_FAILED_BIT;
    }
    (wparam, isize::try_from(cleared.bytes).unwrap_or(isize::MAX))
}

pub fn from_message(wparam: usize, lparam: isize) -> Cleared {
    Cleared {
        entries: wparam as u32,
        failed: ((wparam & !SCAN_FAILED_BIT) >> 32) as u32,
        bytes: lparam.max(0) as u64,
        scan_failed: wparam & SCAN_FAILED_BIT != 0,
    }
}

/// Adds up deletions, one outcome per distinct URL in `found` order. An entry
/// already gone is neither cleared nor failed.
fn tally<'a>(
    found: impl IntoIterator<Item = (&'a str, u64)>,
    outcomes: impl IntoIterator<Item = Result<(), Gone>>,
) -> Cleared {
    let mut cleared = Cleared::default();
    let mut seen = HashSet::new();
    let distinct = found.into_iter().filter(|(url, _)| seen.insert(*url));
    for ((_, bytes), outcome) in distinct.zip(outcomes) {
        match outcome {
            Ok(()) => {
                cleared.entries += 1;
                cleared.bytes += bytes;
            }
            Err(Gone::Already) => {}
            Err(Gone::Failed) => cleared.failed += 1,
        }
    }
    cleared
}

/// Removes every cached lyrics lookup. Runs off the UI thread: the cache is a
/// shared store that can keep a caller waiting.
pub fn clear_lyrics_lookups() -> Cleared {
    let (mut found, complete) = lyrics_entries();
    let mut seen = HashSet::new();
    found.retain(|(url, _)| seen.insert(url.clone()));
    let outcomes: Vec<_> = found
        .iter()
        .map(|(url, _)| {
            let url = HSTRING::from(url.as_str());
            unsafe { DeleteUrlCacheEntryW(PCWSTR(url.as_ptr())) }.map_err(|error| {
                if error.code() == ERROR_FILE_NOT_FOUND.to_hresult() {
                    Gone::Already
                } else {
                    Gone::Failed
                }
            })
        })
        .collect();
    let mut cleared = tally(
        found.iter().map(|(url, bytes)| (url.as_str(), *bytes)),
        outcomes,
    );
    cleared.scan_failed = !complete;
    cleared
}

/// Whether the error that ended a scan means the whole cache was read: only
/// the normal end of the list does (also what an empty cache answers first).
fn read_to_the_end(code: HRESULT) -> bool {
    code == ERROR_NO_MORE_ITEMS.to_hresult()
}

/// URL and size of each cached lyrics lookup, and whether the whole cache was
/// read. Collected first and deleted after the enumeration closes, so deleting
/// can't disturb it.
fn lyrics_entries() -> (Vec<(String, u64)>, bool) {
    let mut found = Vec::new();
    // u64 for the struct's alignment; grown when an entry doesn't fit.
    let mut buffer = vec![0u64; 512];
    let mut find: Option<HANDLE> = None;
    let complete = loop {
        let mut size = (buffer.len() * 8) as u32;
        let info = buffer.as_mut_ptr().cast::<INTERNET_CACHE_ENTRY_INFOW>();
        let step = unsafe {
            match find {
                None => FindFirstUrlCacheEntryW(PCWSTR::null(), Some(info), &mut size).map(|h| {
                    find = Some(h);
                }),
                Some(handle) => FindNextUrlCacheEntryW(handle, Some(info), &mut size),
            }
        };
        match step {
            Ok(()) => {
                let entry = unsafe { &*info };
                let url = unsafe { entry.lpszSourceUrlName.to_string() }.unwrap_or_default();
                if is_lyrics_url(&url) {
                    let bytes = (u64::from(entry.dwSizeHigh) << 32) | u64::from(entry.dwSizeLow);
                    found.push((url, bytes));
                }
            }
            Err(error) if error.code() == ERROR_INSUFFICIENT_BUFFER.to_hresult() => {
                // `size` now holds what the entry needs; the same call is repeated.
                let words = (size as usize).div_ceil(8);
                if words <= buffer.len() || words > 1 << 20 {
                    break false;
                }
                buffer.resize(words, 0);
            }
            Err(error) => break read_to_the_end(error.code()),
        }
    };
    if let Some(handle) = find {
        let _ = unsafe { FindCloseUrlCache(handle) };
    }
    (found, complete)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookups_on_the_lyrics_host_should_match() {
        assert!(
            [
                "https://lrclib.net/api/search?track_name=x",
                "https://LRCLIB.NET/api/get?track_name=y",
                "http://lrclib.net/",
                "https://lrclib.net:443/api/get",
                "https://lrclib.net",
            ]
            .iter()
            .all(|url| is_lyrics_url(url))
        );
    }

    #[test]
    fn other_hosts_should_be_left_alone() {
        assert!(
            ![
                "https://lrclib.net.example.com/api",
                "https://notlrclib.net/api",
                "https://example.com/?u=https://lrclib.net/api",
                "Visited: user@https://lrclib.net/api",
                "ftp://lrclib.net/api",
                "",
            ]
            .iter()
            .any(|url| is_lyrics_url(url))
        );
    }

    #[test]
    fn the_host_should_be_read_past_any_user_name() {
        assert_eq!(
            [
                is_lyrics_url("https://lrclib.net:x@evil.example/page"),
                is_lyrics_url("https://x@lrclib.net/api/get"),
                is_lyrics_url("https://lrclib.net./api/get"),
                // Browsers read a backslash as a slash: the host is lrclib.net.
                is_lyrics_url("https://lrclib.net\\@evil.example/"),
            ],
            [false, true, true, true]
        );
    }

    #[test]
    fn a_scan_that_failed_should_not_say_nothing_was_there() {
        let unread = Cleared {
            scan_failed: true,
            ..Cleared::default()
        };
        let partly = Cleared {
            entries: 2,
            bytes: 64,
            scan_failed: true,
            ..Cleared::default()
        };
        assert_eq!(
            [describe(&unread), describe(&partly)],
            [
                "Couldn't read\nthe web cache",
                "Cleared 2 entries\ncache partly read"
            ]
        );
    }

    #[test]
    fn only_the_end_of_the_list_should_count_as_a_full_read() {
        use windows::Win32::Foundation::ERROR_ACCESS_DENIED;
        assert_eq!(
            [
                read_to_the_end(ERROR_NO_MORE_ITEMS.to_hresult()),
                read_to_the_end(ERROR_ACCESS_DENIED.to_hresult()),
            ],
            [true, false]
        );
    }

    #[test]
    fn an_entry_already_gone_should_not_count_as_in_use() {
        let outcomes = [Ok(()), Err(Gone::Already), Err(Gone::Failed), Ok(())];
        let cleared = tally([("a", 10), ("b", 20), ("c", 30), ("a", 10)], outcomes);
        assert_eq!(
            cleared,
            Cleared {
                entries: 1,
                bytes: 10,
                failed: 1,
                scan_failed: false,
            }
        );
    }

    #[test]
    fn a_clear_should_say_how_much_went() {
        let one = Cleared {
            entries: 1,
            bytes: 31,
            ..Cleared::default()
        };
        let many = Cleared {
            entries: 12,
            bytes: 4_300,
            ..Cleared::default()
        };
        assert_eq!(
            [describe(&one), describe(&many)],
            [
                "Cleared 1 entry\n31 B freed",
                "Cleared 12 entries\n4.2 KB freed"
            ]
        );
    }

    #[test]
    fn an_empty_cache_should_say_so() {
        assert_eq!(describe(&Cleared::default()), "Nothing to clear");
    }

    #[test]
    fn entries_left_behind_should_be_reported() {
        let partly = Cleared {
            entries: 2,
            bytes: 90,
            failed: 1,
            ..Cleared::default()
        };
        let none = Cleared {
            failed: 3,
            ..Cleared::default()
        };
        assert_eq!(
            [describe(&partly), describe(&none)],
            ["Cleared 2 entries\n1 in use", "Couldn't clear\n3 entries"]
        );
    }

    #[test]
    fn a_result_should_cross_a_window_message_unchanged() {
        let cleared = Cleared {
            entries: 70_000,
            bytes: 5_000_000_000,
            failed: 3,
            scan_failed: true,
        };
        let (wparam, lparam) = to_message(&cleared);
        assert_eq!(from_message(wparam, lparam), cleared);
    }

    #[test]
    fn sizes_should_read_in_the_right_unit() {
        assert_eq!(
            [size(0), size(1023), size(1024), size(5 * 1024 * 1024)],
            ["0 B", "1023 B", "1.0 KB", "5.0 MB"]
        );
    }
}
