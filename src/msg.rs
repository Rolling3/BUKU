//! Private window messages and a thread-safe window handle for posting them.

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

/// `wParam` = master volume in thousandths, `lParam` = 1 when muted.
pub const VOLUME: u32 = WM_APP + 1;
/// The default playback device changed (headphones plugged in, etc.).
pub const AUDIO_DEVICE: u32 = WM_APP + 2;
/// Media session or track state changed; read it from the shared media state.
pub const MEDIA: u32 = WM_APP + 3;
/// Lyrics for the current track arrived (or turned out not to exist).
pub const LYRICS: u32 = WM_APP + 4;
/// Notification-area icon callback.
pub const TRAY: u32 = WM_APP + 5;
/// Advance the animation by one frame.
pub const FRAME: u32 = WM_APP + 6;
/// Open the notch menu at the cursor.
pub const SHOW_MENU: u32 = WM_APP + 7;
/// Shell app-bar notifications; used only to learn about full-screen apps.
pub const APPBAR: u32 = WM_APP + 8;
/// Open the text box for a new task.
pub const INPUT_OPEN: u32 = WM_APP + 9;
/// The task text box is done: `wParam` = 1 to save the text, 0 to cancel.
pub const INPUT_CLOSE: u32 = WM_APP + 10;
/// Windows reported a network change.
pub const NETWORK: u32 = WM_APP + 11;
/// Files are being dragged over the notch (`wParam` = 1) or left again (0).
pub const DRAG_OVER: u32 = WM_APP + 12;
/// Files were dropped on the notch; they are waiting in the shared dropped list.
pub const SHELF_DROP: u32 = WM_APP + 13;
/// Start dragging out the file stored in `App::shelf_drag`.
pub const SHELF_DRAG: u32 = WM_APP + 14;
/// Open the file stored in `App::shelf_open`.
pub const SHELF_OPEN: u32 = WM_APP + 15;
/// Open the Settings window.
pub const SETTINGS_OPEN: u32 = WM_APP + 16;
/// The Settings window saved a change; reload `settings.json`.
pub const SETTINGS_CHANGED: u32 = WM_APP + 17;
/// The playing app's own volume changed: `wParam` = volume in thousandths,
/// `lParam` = flags from `audio` and `app_volume`.
pub const APP_VOLUME: u32 = WM_APP + 19;
/// Another window became the foreground window.
pub const FOREGROUND: u32 = WM_APP + 20;
/// To the Settings window: settings.json changed elsewhere (the notch was
/// dragged to another screen); read it again.
pub const SETTINGS_RELOAD: u32 = WM_APP + 21;
/// Take the scale of the screen the notch is on and center it there again,
/// once the move (or the window's creation) is over.
pub const SETTLE: u32 = WM_APP + 22;
/// To the Settings window: clearing cached lyrics lookups finished; the result
/// is packed by `web_cache::to_message`.
pub const CACHE_CLEARED: u32 = WM_APP + 23;
/// Open the Spotify shortcut stored in `App::spotify_open`.
pub const SPOTIFY_OPEN: u32 = WM_APP + 24;
/// Names for Spotify shortcuts arrived; read them from the shared list.
pub const SPOTIFY_NAME: u32 = WM_APP + 25;

/// An `HWND` that can cross threads. Posting to a window from any thread is allowed.
#[derive(Clone, Copy)]
pub struct WindowHandle(pub isize);

impl WindowHandle {
    pub fn new(hwnd: HWND) -> Self {
        Self(hwnd.0 as isize)
    }

    pub fn post(self, message: u32, wparam: usize, lparam: isize) {
        unsafe {
            let _ = PostMessageW(
                Some(HWND(self.0 as *mut _)),
                message,
                WPARAM(wparam),
                LPARAM(lparam),
            );
        }
    }
}
