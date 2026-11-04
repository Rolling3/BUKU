#![windows_subsystem = "windows"]

mod accent;
mod anim;
mod app;
mod app_volume;
mod art;
mod audio;
mod autostart;
mod capslock;
mod display;
mod dragdrop;
mod draw;
mod gfx;
mod input;
mod lyrics;
mod media;
mod msg;
mod pomodoro;
mod settings;
mod settings_window;
mod shelf;
mod store;
mod system;
mod tasks;
mod tray;
mod water;

use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError};
use windows::Win32::System::Ole::OleInitialize;
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::WindowsAndMessaging::FindWindowW;
use windows::core::w;

fn main() {
    // A second copy would draw a second notch on top of the first one.
    // The handle is intentionally kept open for the life of the process.
    let _instance = match unsafe { CreateMutexW(None, true, w!("Local\\BUKU.SingleInstance")) } {
        Ok(handle) => handle,
        Err(_) => return,
    };
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        // Normally that is another BUKU. Another program could also have taken the
        // name first, so with no BUKU window around say why nothing appeared.
        if unsafe { FindWindowW(w!("BUKU.Notch"), None) }.is_err() {
            app::fatal("BUKU could not start because another program is holding its startup lock.");
        }
        return;
    }

    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        // OLE (single-threaded COM plus drag and drop) for the UI thread: the shell,
        // audio and drag-and-drop objects all expect it.
        if OleInitialize(None).is_err() {
            return;
        }
    }

    if let Err(err) = app::run() {
        app::fatal(&format!("BUKU could not start.\n\n{err}"));
    }
}
