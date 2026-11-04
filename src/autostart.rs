//! "Start with Windows", through the per-user Run key.
//!
//! The value is only written when the user flips the switch, never on launch.

use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_SZ, RRF_RT_REG_BINARY,
    RRF_RT_REG_SZ, RegCloseKey, RegDeleteKeyValueW, RegDeleteValueW, RegGetValueW, RegOpenKeyExW,
    RegSetValueExW,
};
use windows::core::{PCWSTR, w};

const RUN_KEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
const VALUE: PCWSTR = w!("BUKU");

/// Where Task Manager records startup apps the user disabled.
const APPROVED_KEY: PCWSTR =
    w!("Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\StartupApproved\\Run");

fn command_line() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    Some(format!("\"{}\"", exe.display()))
}

/// True only when Windows will actually start this copy of BUKU at sign-in.
pub fn is_enabled() -> bool {
    let mut buffer = [0u16; 1024];
    let mut size = std::mem::size_of_val(&buffer) as u32;
    let read = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            RUN_KEY,
            VALUE,
            RRF_RT_REG_SZ,
            None,
            Some(buffer.as_mut_ptr().cast()),
            Some(&mut size),
        )
    };
    if read != ERROR_SUCCESS {
        return false;
    }
    let stored = String::from_utf16_lossy(&buffer[..(size as usize / 2).saturating_sub(1)]);
    // A value left behind by a copy that has since moved doesn't start this one.
    if !command_matches(&stored, command_line().as_deref()) {
        return false;
    }
    !disabled_in_task_manager()
}

/// Windows paths ignore case, and the installer writes the path as Inno Setup expanded it.
fn command_matches(stored: &str, current: Option<&str>) -> bool {
    let unquote = |command: &str| command.trim().trim_matches('"').to_lowercase();
    current.is_some_and(|current| unquote(stored) == unquote(current))
}

/// Task Manager keeps a 12-byte record; an odd first byte means "disabled".
fn disabled_in_task_manager() -> bool {
    let mut data = [0u8; 12];
    let mut size = data.len() as u32;
    let read = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            APPROVED_KEY,
            VALUE,
            RRF_RT_REG_BINARY,
            None,
            Some(data.as_mut_ptr().cast()),
            Some(&mut size),
        )
    };
    read == ERROR_SUCCESS && size > 0 && data[0] & 1 == 1
}

pub fn set_enabled(enabled: bool) -> windows::core::Result<()> {
    let command = command_line().ok_or_else(windows::core::Error::empty)?;
    let mut key = HKEY::default();
    unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            RUN_KEY,
            None,
            KEY_SET_VALUE | KEY_QUERY_VALUE,
            &mut key,
        )
        .ok()?;
        let result = if enabled {
            let wide: Vec<u16> = command.encode_utf16().chain(Some(0)).collect();
            let bytes = std::slice::from_raw_parts(wide.as_ptr().cast::<u8>(), wide.len() * 2);
            let set = RegSetValueExW(key, VALUE, None, REG_SZ, Some(bytes)).ok();
            // Turning it on in BUKU also clears an old "disabled" mark from Task Manager.
            let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, APPROVED_KEY, VALUE);
            set
        } else {
            match RegDeleteValueW(key, VALUE) {
                ERROR_FILE_NOT_FOUND => Ok(()),
                status => status.ok(),
            }
        };
        let _ = RegCloseKey(key);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_matches_should_ignore_case_and_quotes() {
        let current = Some(r#""C:\Program Files\BUKU\buku.exe""#);
        assert!(command_matches(
            r#""c:\program files\buku\BUKU.exe""#,
            current
        ));
        assert!(command_matches(r"C:\Program Files\BUKU\buku.exe", current));
        assert!(!command_matches(r#""C:\Other\buku.exe""#, current));
        assert!(!command_matches(
            r#""C:\Program Files\BUKU\buku.exe""#,
            None
        ));
    }
}
