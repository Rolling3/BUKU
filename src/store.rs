//! Small JSON files in `%APPDATA%\BUKU`.

use serde::Serialize;
use serde::de::DeserializeOwned;
use std::io::Read;
use std::path::{Path, PathBuf};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::UI::Shell::{FOLDERID_RoamingAppData, KF_FLAG_DEFAULT, SHGetKnownFolderPath};

/// Settings and task files are tiny; anything bigger isn't ours.
const MAX_FILE_BYTES: u64 = 1024 * 1024;

pub fn dir() -> Option<PathBuf> {
    roaming_app_data()
        .or_else(|| std::env::var_os("APPDATA").map(PathBuf::from))
        .map(|appdata| appdata.join("BUKU"))
}

/// Asks the shell rather than trusting an environment variable the process was started with.
fn roaming_app_data() -> Option<PathBuf> {
    unsafe {
        let path = SHGetKnownFolderPath(&FOLDERID_RoamingAppData, KF_FLAG_DEFAULT, None).ok()?;
        let folder = path.to_string().ok().map(PathBuf::from);
        CoTaskMemFree(Some(path.as_ptr().cast()));
        folder.filter(|folder| !folder.as_os_str().is_empty())
    }
}

/// The file's bytes, or `None` when it can't be read or is over `MAX_FILE_BYTES`.
fn read_capped(path: &Path) -> Option<Vec<u8>> {
    let file = std::fs::File::open(path).ok()?;
    if file.metadata().ok()?.len() > MAX_FILE_BYTES {
        return None;
    }
    // The file can grow after the check, so cap the read as well.
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES + 1).read_to_end(&mut bytes).ok()?;
    (bytes.len() as u64 <= MAX_FILE_BYTES).then_some(bytes)
}

/// Reads `name`, or the default when it's missing or unreadable.
pub fn load<T: DeserializeOwned + Default>(name: &str) -> T {
    dir()
        .and_then(|dir| read_capped(&dir.join(name)))
        .and_then(|bytes| parse(&bytes))
        .unwrap_or_default()
}

/// Parses JSON, accepting the byte-order mark that editors such as Notepad (and
/// PowerShell's UTF8 encoding) put at the start of a file.
fn parse<T: DeserializeOwned>(bytes: &[u8]) -> Option<T> {
    // Notepad's "Unicode" and PowerShell 5's default encoding are UTF-16 LE.
    if let Some(utf16) = bytes.strip_prefix(b"\xFF\xFE") {
        let units: Vec<u16> = utf16
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_le_bytes(*pair))
            .collect();
        return serde_json::from_str(&String::from_utf16(&units).ok()?).ok();
    }
    let text = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    serde_json::from_slice(text).ok()
}

/// Writes to a temporary file first so a crash mid-write can't leave a truncated file.
pub fn save<T: Serialize>(name: &str, value: &T) -> std::io::Result<()> {
    let dir = dir().ok_or_else(|| std::io::Error::other("APPDATA is not set"))?;
    std::fs::create_dir_all(&dir)?;
    let json = serde_json::to_vec_pretty(value).map_err(std::io::Error::other)?;
    let tmp = dir.join(format!("{name}.tmp"));
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, dir.join(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_should_accept_a_byte_order_mark() {
        let parsed: Option<Vec<u32>> = parse(b"\xEF\xBB\xBF[1, 2]");
        assert_eq!(parsed, Some(vec![1, 2]));
    }

    #[test]
    fn read_capped_should_skip_files_over_the_limit() {
        let dir = std::env::temp_dir().join(format!("buku-store-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let small = dir.join("small.json");
        let big = dir.join("big.json");
        let _ = std::fs::write(&small, b"[1]");
        let _ = std::fs::write(&big, vec![b' '; MAX_FILE_BYTES as usize + 1]);
        let read = (read_capped(&small), read_capped(&big));
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(read, (Some(b"[1]".to_vec()), None));
    }
}

#[cfg(test)]
mod edge_case_tests {
    use super::*;

    #[test]
    fn parse_should_accept_utf16_files_from_notepad() {
        let bytes: Vec<u8> = [0xFF, 0xFE]
            .into_iter()
            .chain("[3, 4]".encode_utf16().flat_map(u16::to_le_bytes))
            .collect();
        let parsed: Option<Vec<u32>> = parse(&bytes);
        assert_eq!(parsed, Some(vec![3, 4]));
    }
}
