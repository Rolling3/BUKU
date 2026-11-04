//! The Shelf: files the user dropped on the notch to keep at hand.
//!
//! Only paths are stored (in `%APPDATA%\BUKU\shelf.json`); the files themselves
//! are never copied or touched.

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};

use windows::Win32::Storage::FileSystem::GetDriveTypeW;
use windows::Win32::System::SystemInformation::GetWindowsDirectoryW;
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::{HSTRING, w};

use crate::store;

const FILE: &str = "shelf.json";
pub const MAX_ITEMS: usize = 12;

#[derive(Debug, Default, Serialize)]
pub struct Shelf {
    items: Vec<PathBuf>,
}

/// Read entry by entry: one that isn't a path is skipped rather than losing the
/// whole shelf, which the next drop would then overwrite.
impl<'de> Deserialize<'de> for Shelf {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        let items = value
            .get("items")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(PathBuf::from)
                    .collect()
            })
            .unwrap_or_default();
        Ok(Self { items })
    }
}

impl Shelf {
    /// Loads the shelf as saved. Files aren't checked here: a network share or USB
    /// drive that isn't connected yet would stall startup, and dropping its files
    /// would lose them for good once the list is saved again.
    pub fn load() -> Self {
        let mut shelf: Self = store::load(FILE);
        shelf.items.truncate(MAX_ITEMS);
        shelf
    }

    fn save(&self) {
        let _ = store::save(FILE, self);
    }

    pub fn items(&self) -> &[PathBuf] {
        &self.items
    }

    pub fn add(&mut self, paths: Vec<PathBuf>) {
        self.items = merge(std::mem::take(&mut self.items), paths);
        self.save();
    }

    pub fn remove(&mut self, path: &Path) {
        self.items.retain(|item| !same_file(item, path));
        self.save();
    }

    pub fn clear(&mut self) {
        self.items.clear();
        self.save();
    }
}

/// Newest first, no duplicates, at most `MAX_ITEMS` (the oldest fall off).
///
/// Windows file names ignore letter case, so `C:\A.txt` and `c:\a.txt` are one
/// file. Names that aren't valid Unicode can't be written to `shelf.json` (and
/// would stop the whole list from saving), so they aren't kept.
fn merge(existing: Vec<PathBuf>, added: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut items: Vec<PathBuf> = Vec::with_capacity(MAX_ITEMS);
    for path in added.into_iter().rev().chain(existing) {
        if path.to_str().is_some() && !items.iter().any(|item| same_file(item, &path)) {
            items.push(path);
        }
    }
    items.truncate(MAX_ITEMS);
    items
}

fn same_file(a: &Path, b: &Path) -> bool {
    a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
}

/// Files that run code when opened. A click on one of these shows it in its folder
/// instead, so a stray click on the Shelf never starts a program.
#[rustfmt::skip]
const RUNNABLE: [&str; 74] = [
    "exe", "com", "scr", "pif", "cpl", "msi", "msp", "msc", "bat", "cmd", "ps1", "psm1", "vbs",
    "vbe", "js", "jse", "wsf", "wsh", "hta", "reg", "jar", "appx", "vb", "ws", "msh", "sct", "url",
    "scf", "lnk", "inf", "chm", "xll", "py", "pyw", "jnlp", "msix", "msixbundle", "appxbundle",
    "application", "appref-ms", "appinstaller", "settingcontent-ms", "rdp", "diagcab",
    "library-ms", "search-ms", "searchconnector-ms", "xbap", "bas", "wsc", "ps1xml", "psc1",
    "psd1", "dll", "ocx", "iso", "img", "vhd", "vhdx", "docm", "xlsm", "pptm", "dotm", "xltm",
    "potm", "ppam", "xlam", "mde", "ade", "adp", "accde", "msu", "wsb", "vsto",
];

/// Windows drops trailing dots and spaces from a file name, so `a.exe.` runs as
/// `a.exe`; the extension is read after the same trimming.
pub fn is_runnable(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    // The text after the last dot, as Windows reads it: unlike `Path::extension`,
    // this counts a name that is only an extension, since `.bat` runs too.
    name.trim_end_matches(['.', ' '])
        .rsplit_once('.')
        .is_some_and(|(_, ext)| {
            // `name.{CLSID}` makes the shell hand the file to that COM class, which
            // can be any program, whatever the name before it looks like.
            (ext.starts_with('{') && ext.ends_with('}'))
                || RUNNABLE.contains(&ext.to_ascii_lowercase().as_str())
        })
}

/// Splits a drive-letter path (`C:\a`, `C:/a`, `\\?\C:\a`) into the drive letter
/// and what follows `X:`. Anything else (a URI such as `file:` or `shell:`, a UNC
/// path, a relative or drive-relative path) is `None`.
fn split_drive(text: &str) -> Option<(char, &str)> {
    let text = text.strip_prefix(r"\\?\").unwrap_or(text);
    let mut chars = text.chars();
    let letter = chars.next().filter(char::is_ascii_alphabetic)?;
    if chars.next()? != ':' {
        return None;
    }
    let rest = chars.as_str();
    rest.starts_with(['\\', '/']).then_some((letter, rest))
}

/// What the Shelf is willing to hand to the shell: an absolute drive-letter path
/// without a second `:` (that would be an alternate data stream, `a.txt:evil.exe`).
fn is_openable_path(path: &Path) -> bool {
    split_drive(&path.as_os_str().to_string_lossy()).is_some_and(|(_, rest)| !rest.contains(':'))
}

/// Opens a Shelf file with its usual app, or shows a program in its folder.
/// Called with no app state borrowed: the shell may show dialogs that pump messages.
pub fn open(path: &Path) {
    if !is_openable_path(path) {
        return;
    }
    let file = HSTRING::from(path.as_os_str());
    unsafe {
        if is_runnable(path) {
            // Windows file names can't hold a quote, so one here comes from an edited
            // shelf.json and would break out of the quoted argument.
            if path.as_os_str().to_string_lossy().contains('"') {
                return;
            }
            let select = HSTRING::from(format!("/select,\"{}\"", path.display()));
            ShellExecuteW(
                None,
                w!("open"),
                &HSTRING::from(explorer().as_path()),
                &select,
                None,
                SW_SHOWNORMAL,
            );
        } else {
            ShellExecuteW(None, w!("open"), &file, None, None, SW_SHOWNORMAL);
        }
    }
}

/// Explorer by full path, so a program named `explorer.exe` elsewhere is never picked.
fn explorer() -> PathBuf {
    let mut buffer = [0u16; 260];
    let len = unsafe { GetWindowsDirectoryW(Some(&mut buffer)) } as usize;
    let windows = if (1..buffer.len()).contains(&len) {
        PathBuf::from(String::from_utf16_lossy(&buffer[..len]))
    } else {
        PathBuf::from(r"C:\Windows")
    };
    windows.join("explorer.exe")
}

const DRIVE_REMOVABLE: u32 = 2;
const DRIVE_FIXED: u32 = 3;
const DRIVE_CDROM: u32 = 5;
const DRIVE_RAMDISK: u32 = 6;

/// True only for a path on a drive that is plainly local. Network paths in any
/// spelling (`\\server\share`, `\\?\UNC\...`, `//server/...`) and mapped network
/// drives count as possibly remote, as does anything that isn't a drive-letter
/// path: they may be slow or unreachable, or contact the host when asked for an icon.
pub fn is_local(path: &Path) -> bool {
    let Some((letter, _)) = split_drive(&path.as_os_str().to_string_lossy()) else {
        return false;
    };
    let root = HSTRING::from(format!("{letter}:\\"));
    let kind = unsafe { GetDriveTypeW(&root) };
    matches!(
        kind,
        DRIVE_REMOVABLE | DRIVE_FIXED | DRIVE_CDROM | DRIVE_RAMDISK
    )
}

/// The name shown under a file's icon.
pub fn display_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(names: &[&str]) -> Vec<PathBuf> {
        names.iter().map(PathBuf::from).collect()
    }

    #[test]
    fn is_runnable_should_catch_programs_and_scripts_in_any_case() {
        let checks =
            ["a.EXE", "b.ps1", "c.txt", "d.pdf", "e"].map(|name| is_runnable(Path::new(name)));
        assert_eq!(checks, [true, true, false, false, false]);
    }

    #[test]
    fn is_runnable_should_catch_shortcuts_and_installers() {
        let checks = [
            "a.url",
            "b.LNK",
            "c.appref-ms",
            "d.settingcontent-ms",
            "e.msixbundle",
        ]
        .map(|name| is_runnable(Path::new(name)));
        assert_eq!(checks, [true; 5]);
    }

    #[test]
    fn is_runnable_should_catch_a_class_id_extension() {
        let checks = [
            "report.{0AFACED1-E828-11D1-9187-B532F1E9575D}",
            "a.pdf.{3050f4d8-98b5-11cf-bb82-00aa00bdce0b}. ",
            "notes.{draft",
        ]
        .map(|name| is_runnable(Path::new(name)));
        assert_eq!(checks, [true, true, false]);
    }

    #[test]
    fn is_runnable_should_ignore_trailing_dots_and_spaces() {
        let checks = [
            "a.exe.",
            "a.exe ",
            "a.exe. .",
            r"C:\x\b.ps1..",
            "c.txt.",
            "d.",
        ]
        .map(|name| is_runnable(Path::new(name)));
        assert_eq!(checks, [true, true, true, true, false, false]);
    }

    #[test]
    fn is_runnable_should_catch_a_name_that_is_only_an_extension() {
        // Windows runs a file named `.bat` as a batch file.
        let checks = [r"C:\x\.bat", ".exe", ".lnk.", ".js ", ".txt", "."]
            .map(|name| is_runnable(Path::new(name)));
        assert_eq!(checks, [true, true, true, true, false, false]);
    }

    #[test]
    fn is_runnable_should_catch_the_added_extensions() {
        let names = [
            "a.rdp",
            "a.diagcab",
            "a.library-ms",
            "a.search-ms",
            "a.searchconnector-ms",
            "a.xbap",
            "a.bas",
            "a.wsc",
            "a.ps1xml",
            "a.psc1",
            "a.psd1",
            "a.dll",
            "a.ocx",
            "a.iso",
            "a.img",
            "a.vhd",
            "a.vhdx",
            "a.docm",
            "a.xlsm",
            "a.pptm",
            "a.dotm",
            "a.xltm",
            "a.potm",
            "a.ppam",
            "a.xlam",
            "a.mde",
            "a.ade",
            "a.adp",
            "a.accde",
            "a.CPL",
            "a.msu",
            "a.wsb",
            "a.vsto",
        ];
        assert!(names.iter().all(|name| is_runnable(Path::new(name))));
    }

    #[test]
    fn split_drive_should_accept_only_drive_letter_forms() {
        let checks = [
            r"C:\a",
            "c:/a",
            r"\\?\C:\a",
            r"\\server\share\a",
            r"\\?\UNC\server\share\a",
            "C:a",
            "C:",
            "file:///C:/a",
            "shell:startup",
            "http://x/a",
            "/a",
            "a.txt",
            "",
        ]
        .map(|text| split_drive(text).map(|(letter, _)| letter));
        let mut expected = [None; 13];
        expected[..3].copy_from_slice(&[Some('C'), Some('c'), Some('C')]);
        assert_eq!(checks, expected);
    }

    #[test]
    fn is_openable_path_should_refuse_uris_streams_and_relative_paths() {
        let checks = [
            r"C:\a\b.txt",
            r"\\?\C:\a\b.txt",
            r"C:\a\b.txt:evil.exe",
            r"C:\a:b\c.txt",
            "file:///C:/a.txt",
            "shell:startup",
            "http://x/a.exe",
            r"\\server\share\a.txt",
            "a.txt",
        ]
        .map(|text| is_openable_path(Path::new(text)));
        assert_eq!(
            checks,
            [true, true, false, false, false, false, false, false, false]
        );
    }

    #[test]
    fn is_local_should_treat_anything_but_a_drive_letter_path_as_remote() {
        let checks = [
            r"\\server\share\a",
            "//server/share/a",
            r"\\?\UNC\server\share\a",
            r"\\.\UNC\server\share\a",
            r"\??\UNC\server\share\a",
            "/a",
            "a.txt",
        ]
        .map(|text| is_local(Path::new(text)));
        assert_eq!(checks, [false; 7]);
    }

    #[test]
    fn is_local_should_accept_the_system_drive() {
        assert!(is_local(&explorer()));
    }

    #[test]
    fn explorer_should_be_a_full_path() {
        assert!(explorer().is_absolute());
    }

    #[test]
    fn merge_should_put_new_files_first_without_duplicates() {
        let merged = merge(paths(&["a", "b"]), paths(&["c", "a"]));
        assert_eq!(merged, paths(&["a", "c", "b"]));
    }

    #[test]
    fn merge_should_drop_the_oldest_beyond_the_limit() {
        let existing: Vec<PathBuf> = (0..MAX_ITEMS)
            .map(|i| PathBuf::from(i.to_string()))
            .collect();
        let merged = merge(existing, paths(&["new"]));
        assert_eq!(
            (merged.len(), merged[0].clone(), merged.last().cloned()),
            (
                MAX_ITEMS,
                PathBuf::from("new"),
                Some(PathBuf::from((MAX_ITEMS - 2).to_string()))
            )
        );
    }
}

#[cfg(test)]
mod load_tests {
    use super::*;

    #[test]
    fn one_invalid_entry_should_not_lose_the_shelf() {
        let json = r#"{"items":["C:\\a.txt",null,7,"C:\\b.txt"]}"#;
        let shelf: Option<Shelf> = serde_json::from_str(json).ok();
        assert_eq!(
            shelf.map(|s| s.items),
            Some(vec![PathBuf::from(r"C:\a.txt"), PathBuf::from(r"C:\b.txt")])
        );
    }
}

#[cfg(test)]
mod edge_case_tests {
    use super::*;

    #[test]
    fn the_same_file_in_different_letter_case_should_be_kept_once() {
        let merged = merge(
            vec![PathBuf::from(r"C:\Docs\A.txt")],
            vec![PathBuf::from(r"c:\docs\a.txt")],
        );
        assert_eq!(merged.len(), 1);
    }

    #[test]
    fn a_path_that_cannot_be_saved_should_not_block_saving_the_others() {
        use std::os::windows::ffi::OsStringExt;
        let odd: Vec<u16> = r"C:\bad\"
            .encode_utf16()
            .chain([0xD800])
            .chain("x.txt".encode_utf16())
            .collect();
        let odd = PathBuf::from(std::ffi::OsString::from_wide(&odd));
        let items = merge(Vec::new(), vec![PathBuf::from(r"C:\a.txt"), odd]);
        assert!(serde_json::to_string(&Shelf { items }).is_ok());
    }
}
