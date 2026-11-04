//! Notification-area icon and its menu.

use windows::Win32::Foundation::{HWND, POINT};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetSystemMetricsForDpi;
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_SETVERSION,
    NOTIFYICON_VERSION_4, NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, GetCursorPos, HICON, IMAGE_ICON, LR_DEFAULTCOLOR,
    LoadImageW, MENU_ITEM_FLAGS, MF_DISABLED, MF_GRAYED, MF_SEPARATOR, MF_STRING, SM_CXSMICON,
    SetForegroundWindow, TPM_BOTTOMALIGN, TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTBUTTON,
    TrackPopupMenu,
};
use windows::core::{PCWSTR, w};

const ICON_ID: u32 = 1;

pub const CMD_SETTINGS: u32 = 10;
pub const CMD_EXIT: u32 = 20;

/// The icon is resource 1, embedded by `build.rs`, at the small-icon size for `dpi`.
/// The caller owns the handle and destroys it with `DestroyIcon`.
pub fn load_icon(dpi: u32) -> HICON {
    unsafe {
        let size = GetSystemMetricsForDpi(SM_CXSMICON, dpi.max(96));
        let module = GetModuleHandleW(None).unwrap_or_default();
        LoadImageW(
            Some(module.into()),
            make_int_resource(1),
            IMAGE_ICON,
            size,
            size,
            LR_DEFAULTCOLOR,
        )
        .map(|handle| HICON(handle.0))
        .unwrap_or_default()
    }
}

/// The icon at a fixed pixel size, for a window class. Lives as long as the process.
pub fn load_icon_sized(size: i32) -> HICON {
    unsafe {
        let module = GetModuleHandleW(None).unwrap_or_default();
        LoadImageW(
            Some(module.into()),
            make_int_resource(1),
            IMAGE_ICON,
            size,
            size,
            LR_DEFAULTCOLOR,
        )
        .map(|handle| HICON(handle.0))
        .unwrap_or_default()
    }
}

/// Win32 identifies numbered resources by passing the number in place of a name pointer.
const fn make_int_resource(id: u16) -> PCWSTR {
    PCWSTR(id as usize as *const u16)
}

fn data(hwnd: HWND) -> NOTIFYICONDATAW {
    NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: ICON_ID,
        ..Default::default()
    }
}

/// Adds the icon. Also called again when Explorer restarts and the old icon is gone.
pub fn add(hwnd: HWND, callback_message: u32, icon: HICON) {
    let mut nid = data(hwnd);
    nid.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP | NIF_SHOWTIP;
    nid.uCallbackMessage = callback_message;
    nid.hIcon = icon;
    let tip: Vec<u16> = "BUKU".encode_utf16().collect();
    nid.szTip[..tip.len()].copy_from_slice(&tip);
    unsafe {
        let _ = Shell_NotifyIconW(NIM_ADD, &nid);
        nid.Anonymous.uVersion = NOTIFYICON_VERSION_4;
        let _ = Shell_NotifyIconW(NIM_SETVERSION, &nid);
    }
}

pub fn remove(hwnd: HWND) {
    let nid = data(hwnd);
    unsafe {
        let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
    }
}

/// Shows the menu at the cursor and returns the chosen command, or 0.
pub fn show_menu(hwnd: HWND) -> u32 {
    unsafe {
        let Ok(menu) = CreatePopupMenu() else {
            return 0;
        };
        let items: [(MENU_ITEM_FLAGS, u32, PCWSTR); 5] = [
            (MF_STRING | MF_DISABLED | MF_GRAYED, 0, w!("BUKU")),
            (MF_SEPARATOR, 0, PCWSTR::null()),
            (MF_STRING, CMD_SETTINGS, w!("Settings…")),
            (MF_SEPARATOR, 0, PCWSTR::null()),
            (MF_STRING, CMD_EXIT, w!("Exit")),
        ];
        for (flags, id, text) in items {
            let _ = AppendMenuW(menu, flags, id as usize, text);
        }

        let mut point = POINT::default();
        let _ = GetCursorPos(&mut point);
        // Without this the menu doesn't close when the user clicks elsewhere.
        let _ = SetForegroundWindow(hwnd);
        let chosen = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON | TPM_BOTTOMALIGN,
            point.x,
            point.y,
            None,
            hwnd,
            None,
        );
        let _ = DestroyMenu(menu);
        chosen.0 as u32
    }
}
