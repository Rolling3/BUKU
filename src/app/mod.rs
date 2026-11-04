//! The notch window: state, input, timers and the frame loop.
//!
//! The thread sleeps in `GetMessageW` whenever nothing is animating. Frames are
//! only produced while a spring or fade is moving, and timers only exist while
//! something on screen is time-dependent (a HUD waiting to hide, a lyric line
//! waiting to change), so an idle notch uses no CPU.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HANDLE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Globalization::{GetDateFormatEx, GetTimeFormatEx, TIME_NOSECONDS};
use windows::Win32::Graphics::Direct2D::ID2D1Bitmap1;
use windows::Win32::Graphics::Dwm::DwmFlush;
use windows::Win32::Graphics::Gdi::{
    CreateRectRgn, GetMonitorInfoW, MONITOR_DEFAULTTONULL, MONITOR_DEFAULTTOPRIMARY, MONITORINFO,
    MonitorFromWindow, ScreenToClient, SetWindowRgn,
};
use windows::Win32::System::Diagnostics::Debug::MessageBeep;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Ole::{
    DROPEFFECT, DROPEFFECT_MOVE, IDropTarget, RegisterDragDrop, RevokeDragDrop,
};
use windows::Win32::System::Power::{
    HPOWERNOTIFY, POWERBROADCAST_SETTING, RegisterPowerSettingNotification,
    UnregisterPowerSettingNotification,
};
use windows::Win32::System::SystemServices::MK_LBUTTON;
use windows::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetDoubleClickTime, GetKeyState, ReleaseCapture, SetCapture, TME_LEAVE, TRACKMOUSEEVENT,
    TrackMouseEvent, VK_CAPITAL, VK_CONTROL, VK_LWIN, VK_MENU, VK_SHIFT, VK_SPACE,
};
use windows::Win32::UI::Input::{
    GetRawInputData, HRAWINPUT, RAWINPUT, RAWINPUTDEVICE, RAWINPUTHEADER, RID_INPUT,
    RIDEV_INPUTSINK, RIM_TYPEKEYBOARD, RegisterRawInputDevices,
};
use windows::Win32::UI::Shell::{
    ABM_NEW, ABM_REMOVE, ABN_FULLSCREENAPP, APPBARDATA, SHAppBarMessage,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CREATESTRUCTW, CS_HREDRAW, CS_VREDRAW, CreateWindowExW, DEVICE_NOTIFY_WINDOW_HANDLE,
    DefWindowProcW, DestroyIcon, DestroyWindow, DispatchMessageW, EVENT_SYSTEM_FOREGROUND,
    FindWindowW, GWL_STYLE, GWLP_USERDATA, GetClassNameW, GetCursorPos, GetForegroundWindow,
    GetMessageW, GetWindowLongPtrW, GetWindowLongW, GetWindowRect, HICON, HWND_TOPMOST, IDC_ARROW,
    IsIconic, IsZoomed, KillTimer, LoadCursorW, MA_NOACTIVATE, MB_ICONASTERISK, MB_ICONERROR,
    MB_OK, MSG, MessageBoxW, PBT_POWERSETTINGCHANGE, PostQuitMessage, RI_KEY_BREAK,
    RegisterClassExW, RegisterWindowMessageW, SW_HIDE, SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SetTimer,
    SetWindowLongPtrW, SetWindowPos, ShowWindow, TranslateMessage, WHEEL_DELTA,
    WINEVENT_OUTOFCONTEXT, WM_CAPTURECHANGED, WM_CLOSE, WM_CONTEXTMENU, WM_CREATE, WM_DESTROY,
    WM_DISPLAYCHANGE, WM_DPICHANGED, WM_ENDSESSION, WM_INPUT, WM_LBUTTONDOWN, WM_LBUTTONUP,
    WM_MOUSEACTIVATE, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_NCCREATE, WM_POWERBROADCAST, WM_RBUTTONUP,
    WM_TIMER, WNDCLASSEXW, WS_CAPTION, WS_EX_NOACTIVATE, WS_EX_NOREDIRECTIONBITMAP,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP, WS_VISIBLE, WindowFromPoint,
};
use windows::core::{GUID, Result, w};

use crate::accent;
use crate::anim::{Fade, Spring};
use crate::app_volume::{APP_VOLUME_GONE, AppVolume};
use crate::audio::{Audio, VOLUME_MUTED, VOLUME_OWN, volume_generation};
use crate::capslock::{self, CapsHook};
use crate::display;
use crate::dragdrop::{self, DropTarget, Dropped};
use crate::draw::{
    self, Button, Card, Content, FocusCard, Hud, MediaCard, Page, PageBody, PageKind, SHELF_TILES,
    Scene, ShelfCard, TASK_ROWS, TasksCard, WaterCard,
};
use crate::gfx::Gfx;
use crate::input::TaskInput;
use crate::lyric_sync::{self, LyricSync, Step};
use crate::lyrics::{self, Lyrics};
use crate::media::{self, Media};
use crate::msg::{self, WindowHandle};
use crate::pomodoro::{self, Phase, Pomodoro};
use crate::settings::Settings;
use crate::shelf::{self, Shelf};
use crate::system;
use crate::tasks::TaskList;
use crate::water::{self, WaterLog};
use crate::{settings_window, tray};
use windows::Networking::Connectivity::{NetworkInformation, NetworkStatusChangedEventHandler};

mod events;
mod view;

/// The power-policy brightness setting; changes when the user presses brightness
/// keys or moves the Windows slider. Adaptive brightness doesn't touch it, so the
/// HUD won't pop up on its own when the room gets darker.
const GUID_VIDEO_BRIGHTNESS_SETTING: GUID = GUID::from_u128(0xaded5e82_b909_4619_9949_f5d71dac0bcb);

const GUID_ACDC_POWER_SOURCE: GUID = GUID::from_u128(0x5d3e9a59_e9d5_4b00_a6bd_ff34ff516548);
const GUID_BATTERY_PERCENTAGE_REMAINING: GUID =
    GUID::from_u128(0xa7ad8041_b45a_4cae_87a3_eecbb468a9e1);
/// Charge levels (percent) at which an unplugged laptop gets a warning.
const LOW_BATTERY_LEVELS: [u32; 2] = [20, 10];

/// Releasing one of these may have switched the keyboard language (Alt+Shift,
/// Ctrl+Shift, Win+Space), so the language is checked after it.
const LANGUAGE_SWITCH_KEYS: [u16; 5] = [VK_SHIFT.0, VK_MENU.0, VK_CONTROL.0, VK_SPACE.0, VK_LWIN.0];
const LANGUAGE_SETTLE_MS: u32 = 150;
/// How far the pointer moves with the button down before a press becomes a drag.
const DRAG_START_DIPS: f32 = 5.0;
const NETWORK_SETTLE_MS: u32 = 1500;

/// Not exported by the Win32 bindings alongside the other mouse messages.
const WM_MOUSELEAVE: u32 = 0x02A3;

const TIMER_HUD: usize = 1;
const TIMER_HOVER_IN: usize = 2;
const TIMER_HOVER_OUT: usize = 3;
const TIMER_LYRIC: usize = 4;
const TIMER_PROGRESS: usize = 5;
const TIMER_REBUILD: usize = 6;
const TIMER_POMODORO: usize = 7;
const TIMER_WATER: usize = 8;
const TIMER_NETWORK: usize = 9;
const TIMER_LANGUAGE: usize = 10;
const TIMER_FULLSCREEN: usize = 11;
const TIMER_FULLSCREEN_LATE: usize = 12;
const TIMER_EQUALIZER: usize = 13;

/// About 30 frames a second for the equalizer: smooth, and light on the GPU.
const EQUALIZER_FRAME_MS: u32 = 33;
/// The equalizer's clock wraps here, so `f32` keeps its precision on long runs.
const EQUALIZER_LOOP_SECS: f32 = 3600.0;

/// The shell says "full screen" a moment before the window that caused it is
/// in front (after Win+D, or closing the last window), and a game may resize
/// itself to the screen a while later, so look again after these.
const FULLSCREEN_RECHECK_MS: u32 = 400;
const FULLSCREEN_LATE_RECHECK_MS: u32 = 1500;

const HUD_MS: u32 = 1600;
/// Pomodoro and water notices stay longer than a volume change: they're news.
const NOTICE_MS: u32 = 5000;
/// Delay before the notch opens under the pointer, so passing over it on the way
/// to a browser tab doesn't open it.
const HOVER_IN_MS: u32 = 160;
const HOVER_OUT_MS: u32 = 280;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum View {
    Collapsed,
    Hud,
    /// The hover card with its Music, Focus and Water pages.
    Card,
}

struct App {
    hwnd: HWND,
    gfx: Option<Gfx>,
    scale: f32,
    settings: Settings,
    fetch_lyrics: Arc<AtomicBool>,
    audio: Option<Audio>,
    media: Media,
    power_notify: Vec<HPOWERNOTIFY>,
    /// Token of the WinRT network-change subscription.
    network_token: Option<i64>,
    /// Registered with the shell as an app bar, to hear about full-screen apps.
    appbar: bool,
    /// A full-screen app (game, video) covers the notch's monitor; stay hidden.
    fullscreen: bool,
    /// When BUKU started; the equalizer's clock.
    started: Instant,
    /// The equalizer timer is running.
    equalizer_running: bool,
    /// What the shell last said: an app on this screen is full screen. The notch
    /// hides only while the window in front also really covers the screen.
    shell_fullscreen: bool,
    /// Tells the notch when another window comes to the front.
    foreground_hook: HWINEVENTHOOK,
    taskbar_created: u32,
    tray_icon: HICON,

    width: Spring,
    height: Spring,
    content: Fade,
    shown: View,
    animating: bool,
    last_frame: Instant,

    hud: Option<Hud>,
    /// The HUD being drawn. Kept while the HUD fades out after `hud` is cleared.
    shown_hud: Option<Hud>,
    last_volume: Option<(usize, bool)>,
    last_brightness: Option<u32>,
    caps_on: bool,
    caps_down: bool,

    hover: bool,
    /// The pin on the card is on: the card stays open without the pointer.
    pinned: bool,
    tracking_mouse: bool,
    hot: Option<Button>,
    pressed: Option<Button>,
    /// The playing app's audio sessions, which the volume bar controls.
    app_volume: Option<AppVolume>,
    /// The player app id `app_volume` was last looked up for, and whether it
    /// was playing then.
    app_volume_for: (String, bool),
    /// Counts lookups, so notifications from the sessions of an earlier one are
    /// ignored.
    app_volume_generation: u32,
    /// The playing app's volume (0 to 1000) and mute, as the bar shows it.
    app_level: Option<(usize, bool)>,
    /// The volume bar is being dragged; the window has the mouse capture.
    volume_drag: bool,
    /// The left button went down on the notch away from any button; moving
    /// far enough from there starts dragging the notch to another screen.
    drag_armed: bool,
    /// The notch is being dragged between screens; the window has the capture.
    screen_drag: bool,
    /// Inside `move_to_screen`'s window move; see WM_DPICHANGED.
    moving: bool,
    /// The app bar registers again once a move to another screen is over.
    reregister_appbar: bool,
    /// The drag ended; wndproc releases the capture once the App isn't borrowed.
    release_capture: bool,

    lyric_index: Option<usize>,
    /// Album art as a bitmap on the current device, tagged with `MediaState::art_id`.
    art: Option<(u64, ID2D1Bitmap1)>,
    /// Window region last applied, in pixels: (left, right, bottom).
    region: Option<(i32, i32, i32)>,

    page: Page,
    pomodoro: Pomodoro,
    water: WaterLog,
    /// Per-song lyric offsets set on the Music card.
    lyric_sync: LyricSync,
    /// Seconds since 1970 when BUKU started, or the last water reminder was shown;
    /// the next reminder is counted from here when it's later than the last glass.
    water_since: u64,
    tasks: TaskList,
    /// The text box for a new task, while it's open.
    input: Option<TaskInput>,
    /// When a task was last ticked or deleted, to swallow the second click of a double-click.
    tasks_changed_at: Option<Instant>,

    // Last known system state; the first reading is a baseline, not a notice.
    on_ac: Option<bool>,
    battery: Option<u32>,
    /// Whether the AC/DC and battery notifications were registered (laptops only).
    power_hud_registered: bool,
    network: Option<system::NetworkState>,
    output: Option<(String, bool)>,
    language: Option<u16>,

    shelf: Shelf,
    /// Filled by the drop target, emptied when `SHELF_DROP` arrives.
    dropped: Dropped,
    drop_target: Option<IDropTarget>,
    /// Files are being dragged over the notch; keeps the Shelf open.
    drag_over: bool,
    /// Shelf file icons on the current device, by path, with when each was looked
    /// up: `None` marks one that couldn't be loaded, tried again after a while.
    shelf_icons: HashMap<PathBuf, (Option<ID2D1Bitmap1>, Instant)>,
    /// The file behind the Shelf tile that was clicked, resolved at click time so a
    /// drop landing before `SHELF_OPEN` arrives can't shift the tile onto another file.
    shelf_open: Option<PathBuf>,
    /// Same for a drag that is about to start.
    shelf_drag: Option<PathBuf>,
    /// Where the left button went down, to tell a click from a drag.
    press_point: (f32, f32),
    caps_hook: CapsHook,
}

pub fn fatal(message: &str) {
    let text: Vec<u16> = message.encode_utf16().chain(Some(0)).collect();
    unsafe {
        MessageBoxW(
            None,
            windows::core::PCWSTR(text.as_ptr()),
            w!("BUKU"),
            MB_OK | MB_ICONERROR,
        );
    }
}

thread_local! {
    /// Why `App::new` failed, so `run` can report it once with the real cause.
    static STARTUP_ERROR: std::cell::RefCell<Option<windows::core::Error>> = const { std::cell::RefCell::new(None) };
}

pub fn run() -> Result<()> {
    unsafe {
        let instance = GetModuleHandleW(None)?;
        let class = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            lpszClassName: w!("BUKU.Notch"),
            ..Default::default()
        };
        if RegisterClassExW(&class) == 0 {
            return Err(windows::core::Error::from_thread());
        }
        CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_NOREDIRECTIONBITMAP,
            w!("BUKU.Notch"),
            w!("BUKU"),
            WS_POPUP | WS_VISIBLE,
            0,
            0,
            1,
            1,
            None,
            None,
            Some(instance.into()),
            None,
        )
        .map_err(|err| {
            STARTUP_ERROR
                .with(|cell| cell.borrow_mut().take())
                .unwrap_or(err)
        })?;

        let mut message = MSG::default();
        while GetMessageW(&mut message, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
    Ok(())
}

unsafe extern "system" fn wndproc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    unsafe {
        if message == WM_NCCREATE {
            return DefWindowProcW(hwnd, message, wparam, lparam);
        }
        if message == WM_CREATE {
            let _ = lparam.0 as *const CREATESTRUCTW;
            return match App::new(hwnd) {
                Ok(app) => {
                    SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(Box::new(app)) as isize);
                    LRESULT(0)
                }
                Err(err) => {
                    STARTUP_ERROR.with(|cell| *cell.borrow_mut() = Some(err));
                    LRESULT(-1)
                }
            };
        }
        let app = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut App;
        if app.is_null() {
            if message == WM_DESTROY {
                PostQuitMessage(0);
            }
            return DefWindowProcW(hwnd, message, wparam, lparam);
        }
        if message == WM_DESTROY {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            drop(Box::from_raw(app));
            PostQuitMessage(0);
            return LRESULT(0);
        }
        // Creating and destroying the text box moves keyboard focus, which sends
        // messages back here; like the menu, that happens with no borrow of the App.
        if message == msg::INPUT_OPEN {
            if (*app).input.is_none() && (*app).can_add_task() {
                let (x, y, width, height, scale) = (*app).input_rect();
                let input = TaskInput::open(hwnd, x, y, width, height, scale).ok();
                let app = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut App;
                if !app.is_null() {
                    (*app).input = input;
                }
            }
            return LRESULT(0);
        }
        if message == msg::INPUT_CLOSE {
            if let Some(input) = (*app).input.take() {
                let text = (wparam.0 == 1).then(|| input.text());
                drop(input);
                let app = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut App;
                if !app.is_null() {
                    (*app).on_input_closed(text);
                }
            }
            return LRESULT(0);
        }
        if message == msg::SHELF_DRAG {
            // Dragging out is modal (OLE runs its own message loop), so it runs with
            // no borrow of the App held.
            if let Some(path) = (*app).shelf_drag.take() {
                let effect = dragdrop::drag_out(&path);
                let app = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut App;
                if !app.is_null() {
                    (*app).after_drag_out(&path, effect);
                }
            }
            return LRESULT(0);
        }
        // Signing out, shutting down, or an installer closing BUKU through Restart
        // Manager: leave cleanly so the tray icon and app-bar registration go away.
        if message == WM_ENDSESSION && wparam.0 != 0 {
            let _ = DestroyWindow(hwnd);
            return LRESULT(0);
        }
        if message == msg::SETTINGS_OPEN {
            settings_window::open(hwnd);
            return LRESULT(0);
        }
        if message == msg::SHELF_OPEN {
            if let Some(path) = (*app).shelf_open.take() {
                shelf::open(&path);
            }
            return LRESULT(0);
        }
        if message == msg::SHOW_MENU {
            // The menu runs a modal message loop that re-enters this procedure, so no
            // borrow of the App may be held across it.
            let command = tray::show_menu(hwnd);
            let app = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut App;
            if !app.is_null() {
                (*app).apply_command(command);
            }
            return LRESULT(0);
        }
        let result = match (*app).handle(message, wparam, lparam) {
            Some(result) => result,
            None => DefWindowProcW(hwnd, message, wparam, lparam),
        };
        // ReleaseCapture sends WM_CAPTURECHANGED straight back here, so it runs
        // with no borrow of the App held.
        let app = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut App;
        if !app.is_null() && std::mem::take(&mut (*app).release_capture) {
            let _ = ReleaseCapture();
        }
        result
    }
}

impl App {
    fn new(hwnd: HWND) -> Result<Self> {
        let settings = Settings::load();
        let (settings_focus, settings_break) = (settings.focus_minutes, settings.break_minutes);
        let fetch_lyrics = Arc::new(AtomicBool::new(settings.fetch_lyrics));
        let window = WindowHandle::new(hwnd);
        let scale = unsafe { GetDpiForWindow(hwnd) }.max(96) as f32 / 96.0;

        let mut app = Self {
            hwnd,
            gfx: None,
            scale,
            settings,
            audio: Audio::new(window).ok(),
            media: Media::start(window, fetch_lyrics.clone()),
            fetch_lyrics,
            power_notify: Vec::new(),
            network_token: None,
            appbar: false,
            fullscreen: false,
            started: Instant::now(),
            equalizer_running: false,
            shell_fullscreen: false,
            foreground_hook: HWINEVENTHOOK::default(),
            taskbar_created: unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) },
            tray_icon: tray::load_icon(unsafe { GetDpiForWindow(hwnd) }),
            width: Spring::new(draw::COLLAPSED_W),
            height: Spring::new(draw::COLLAPSED_H),
            content: Fade::new(1.0, 0.09),
            shown: View::Collapsed,
            animating: false,
            last_frame: Instant::now(),
            hud: None,
            shown_hud: None,
            last_volume: None,
            last_brightness: None,
            caps_on: unsafe { GetKeyState(VK_CAPITAL.0 as i32) } & 1 != 0,
            caps_down: false,
            hover: false,
            pinned: false,
            tracking_mouse: false,
            hot: None,
            pressed: None,
            app_volume: None,
            app_volume_for: (String::new(), false),
            app_volume_generation: 0,
            app_level: None,
            volume_drag: false,
            drag_armed: false,
            screen_drag: false,
            moving: false,
            reregister_appbar: false,
            release_capture: false,
            lyric_index: None,
            art: None,
            region: None,
            page: Page::default(),
            pomodoro: Pomodoro::new(settings_focus, settings_break),
            water: WaterLog::load(),
            lyric_sync: LyricSync::load(),
            water_since: water::unix_now(),
            tasks: TaskList::load(),
            input: None,
            tasks_changed_at: None,
            on_ac: None,
            battery: None,
            power_hud_registered: false,
            network: Some(system::network()),
            output: None,
            language: Some(system::keyboard_language()),
            shelf: Shelf::load(),
            dropped: Dropped::default(),
            drop_target: None,
            drag_over: false,
            shelf_icons: HashMap::new(),
            shelf_open: None,
            shelf_drag: None,
            press_point: (0.0, 0.0),
            caps_hook: CapsHook::new(),
        };

        app.place_window();
        // The window starts on the main screen. Placed on another with a
        // different scale, it can't hear about it yet (this is still WM_CREATE),
        // so take that screen's scale now, and settle there once it exists.
        if app.sync_scale() {
            app.place_window();
        }
        window.post(msg::SETTLE, 0, 0);
        app.gfx = Some(Gfx::new(
            hwnd,
            app.canvas_px().0,
            app.canvas_px().1,
            96.0 * app.scale,
        )?);
        tray::add(hwnd, msg::TRAY, app.tray_icon);

        unsafe {
            let mut settings = vec![GUID_VIDEO_BRIGHTNESS_SETTING];
            // Laptops only: desktops have no battery to report on.
            if system::battery_percent().is_some() {
                settings.extend([GUID_ACDC_POWER_SOURCE, GUID_BATTERY_PERCENTAGE_REMAINING]);
                app.power_hud_registered = true;
            }
            app.power_notify = settings
                .iter()
                .filter_map(|setting| {
                    RegisterPowerSettingNotification(
                        HANDLE(hwnd.0),
                        setting,
                        DEVICE_NOTIFY_WINDOW_HANDLE,
                    )
                    .ok()
                })
                .collect();
            // Raw input lets BUKU see Caps Lock presses without a keyboard hook: it
            // observes keys after the system has delivered them and can never block one.
            let keyboard = RAWINPUTDEVICE {
                usUsagePage: 0x01,
                usUsage: 0x06,
                dwFlags: RIDEV_INPUTSINK,
                hwndTarget: hwnd,
            };
            app.appbar = register_appbar(hwnd);
            let _ =
                RegisterRawInputDevices(&[keyboard], std::mem::size_of::<RAWINPUTDEVICE>() as u32);
        }

        app.output = app.audio.as_ref().and_then(Audio::default_device);
        NOTCH_WINDOW.store(hwnd.0 as isize, Ordering::Relaxed);
        app.foreground_hook = unsafe {
            SetWinEventHook(
                EVENT_SYSTEM_FOREGROUND,
                EVENT_SYSTEM_FOREGROUND,
                None,
                Some(on_foreground_event),
                0,
                0,
                // Not WINEVENT_SKIPOWNPROCESS: the Settings window is ours and can
                // come to the front too.
                WINEVENT_OUTOFCONTEXT,
            )
        };
        app.last_volume = app.audio.as_ref().and_then(Audio::level);
        app.network_token = NetworkInformation::NetworkStatusChanged(
            &NetworkStatusChangedEventHandler::new(move |_| {
                window.post(msg::NETWORK, 0, 0);
                Ok(())
            }),
        )
        .ok();
        let target = DropTarget::create(window, app.dropped.clone());
        if unsafe { RegisterDragDrop(hwnd, &target) }.is_ok() {
            app.drop_target = Some(target);
        }
        app.caps_hook
            .apply(app.settings.caps_lock, &app.settings.caps_custom);
        accent::set_current(app.settings.accent);
        app.schedule_water();
        app.update_region();
        app.render();
        Ok(app)
    }

    fn handle(&mut self, message: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
        match message {
            msg::FRAME => self.frame(),
            msg::VOLUME => self.on_volume(wparam.0, lparam.0),
            msg::APP_VOLUME => self.on_app_volume(lparam.0),
            msg::AUDIO_DEVICE => self.on_audio_device_changed(),
            msg::NETWORK => self.on_network_event(),
            msg::SETTINGS_CHANGED => self.reload_settings(),
            msg::DRAG_OVER => self.on_drag_over(wparam.0 == 1),
            msg::SHELF_DROP => self.on_shelf_drop(),
            msg::MEDIA | msg::LYRICS => self.on_media_changed(),
            msg::TRAY => self.on_tray(lparam),
            msg::APPBAR => self.on_appbar(wparam, lparam),
            msg::FOREGROUND => self.update_fullscreen(false),
            msg::SETTLE => self.settle(),
            WM_POWERBROADCAST if wparam.0 == PBT_POWERSETTINGCHANGE as usize => {
                self.on_power_setting(lparam);
                return Some(LRESULT(1));
            }
            WM_INPUT => {
                self.on_raw_input(lparam);
                // Raw input must still reach DefWindowProc so the system can clean up.
                return None;
            }
            WM_TIMER => self.on_timer(wparam.0),
            WM_MOUSEMOVE => self.on_mouse_move(wparam, lparam),
            WM_MOUSELEAVE => self.on_mouse_leave(),
            WM_MOUSEWHEEL => {
                if !self.on_mouse_wheel(wparam, lparam) {
                    return None;
                }
            }
            // Capture taken away (Alt+Tab, a menu): the drag is over.
            WM_CAPTURECHANGED if self.volume_drag => {
                self.volume_drag = false;
                self.after_volume_drag();
            }
            // `lparam` is the window that has the mouse now; this one taking it
            // again is no loss.
            WM_CAPTURECHANGED if lparam.0 == self.hwnd.0 as isize => return None,
            WM_CAPTURECHANGED if self.screen_drag => self.end_screen_drag(false),
            WM_CAPTURECHANGED if self.drag_armed => self.drag_armed = false,
            WM_LBUTTONDOWN => {
                self.pressed = self.button_at(lparam);
                self.press_point = self.pointer_dips(lparam);
                self.drag_armed = self.pressed.is_none();
                if self.drag_armed {
                    // Held from the press, so a quick flick off the notch still
                    // reaches here and can start a drag to another screen.
                    unsafe {
                        SetCapture(self.hwnd);
                    }
                }
                if self.pressed == Some(Button::Volume) {
                    self.begin_volume_drag(lparam);
                }
                if self.shown == View::Collapsed {
                    self.set_hover(true);
                }
            }
            WM_LBUTTONUP => {
                if std::mem::take(&mut self.drag_armed) {
                    // A plain click: let go of the mouse held since the press. If
                    // it came up just off the notch, no move or leave will follow.
                    self.release_capture = true;
                    if !self.pointer_over_window() {
                        self.on_mouse_leave();
                    }
                }
                if self.screen_drag {
                    self.end_screen_drag(true);
                    return Some(LRESULT(0));
                }
                if self.volume_drag {
                    self.pressed = None;
                    self.end_volume_drag();
                    return Some(LRESULT(0));
                }
                let released = self.button_at(lparam);
                if released.is_some() && released == self.pressed.take() {
                    self.on_button(released);
                }
            }
            WM_RBUTTONUP => WindowHandle::new(self.hwnd).post(msg::SHOW_MENU, 0, 0),
            // Clicks must never take focus away from the window the user is typing in.
            WM_MOUSEACTIVATE => return Some(LRESULT(MA_NOACTIVATE as isize)),
            // Sent from inside the move's own SetWindowPos, with this App still in
            // use there; SETTLE takes the new scale once the move is over.
            WM_DPICHANGED if self.moving => {}
            WM_DPICHANGED => {
                // Windows tops out at 500%; a larger value can only come from a forged message.
                self.scale = (wparam.0 & 0xffff).clamp(96, 480) as f32 / 96.0;
                self.on_display_changed();
            }
            WM_DISPLAYCHANGE => self.on_display_changed(),
            _ if message == self.taskbar_created && message != 0 => {
                // A restarted Explorer has forgotten the tray icon and the app bar, and
                // can't tell us a full-screen app closed, so start out visible again.
                tray::add(self.hwnd, msg::TRAY, self.tray_icon);
                // A second ABM_NEW for a window that is still registered fails, so
                // drop the old registration first.
                if self.appbar {
                    remove_appbar(self.hwnd);
                }
                self.appbar = register_appbar(self.hwnd);
                self.shell_fullscreen = false;
                if self.fullscreen {
                    self.fullscreen = false;
                    unsafe {
                        let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
                    }
                    self.place_window();
                }
            }
            _ => return None,
        }
        Some(LRESULT(0))
    }

    fn on_timer(&mut self, id: usize) {
        match id {
            TIMER_FULLSCREEN | TIMER_FULLSCREEN_LATE => {
                unsafe {
                    let _ = KillTimer(Some(self.hwnd), id);
                }
                self.update_fullscreen(true);
            }
            TIMER_HUD => {
                unsafe {
                    let _ = KillTimer(Some(self.hwnd), TIMER_HUD);
                }
                self.hud = None;
                self.retarget();
            }
            TIMER_HOVER_IN => self.set_hover(true),
            TIMER_HOVER_OUT => self.set_hover(false),
            TIMER_LYRIC => {
                let before = self.lyric_index;
                self.schedule_lyric();
                if self.lyric_index != before {
                    // The new line shows at once; the notch resizes around it.
                    self.retarget();
                    if !self.animating {
                        self.render();
                    }
                }
            }
            TIMER_PROGRESS | TIMER_EQUALIZER if !self.animating => self.render(),
            TIMER_POMODORO => self.on_pomodoro_tick(),
            TIMER_WATER => self.on_water_due(),
            TIMER_NETWORK => self.check_network(),
            TIMER_LANGUAGE => self.check_language(),
            TIMER_REBUILD => {
                unsafe {
                    let _ = KillTimer(Some(self.hwnd), TIMER_REBUILD);
                }
                self.region = None;
                self.update_region();
                self.render();
            }
            _ => {}
        }
    }

    // ----- tray -----

    fn on_tray(&mut self, lparam: LPARAM) {
        // With NOTIFYICON_VERSION_4 the event is in the low word.
        let event = (lparam.0 & 0xffff) as u32;
        const NIN_SELECT: u32 = 0x0400;
        const NIN_KEYSELECT: u32 = 0x0401;
        if matches!(event, WM_CONTEXTMENU | NIN_SELECT | NIN_KEYSELECT) {
            WindowHandle::new(self.hwnd).post(msg::SHOW_MENU, 0, 0);
        }
    }

    fn on_appbar(&mut self, wparam: WPARAM, lparam: LPARAM) {
        if wparam.0 as u32 != ABN_FULLSCREENAPP {
            return;
        }
        self.shell_fullscreen = lparam.0 != 0;
        self.update_fullscreen(false);
        if self.shell_fullscreen {
            unsafe {
                SetTimer(
                    Some(self.hwnd),
                    TIMER_FULLSCREEN,
                    FULLSCREEN_RECHECK_MS,
                    None,
                );
                SetTimer(
                    Some(self.hwnd),
                    TIMER_FULLSCREEN_LATE,
                    FULLSCREEN_LATE_RECHECK_MS,
                    None,
                );
            }
        }
    }

    /// Puts the notch on the screen the settings name. The shell tells an app bar
    /// about full-screen apps on its own screen, so it registers again there.
    /// Takes the scale of the screen the window is on now; true when it changed.
    fn sync_scale(&mut self) -> bool {
        let scale = unsafe { GetDpiForWindow(self.hwnd) }.clamp(96, 480) as f32 / 96.0;
        std::mem::replace(&mut self.scale, scale) != scale
    }

    fn move_to_screen(&mut self) {
        // What the shell said about full-screen apps was about the old screen.
        self.shell_fullscreen = false;
        if self.fullscreen {
            self.fullscreen = false;
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
            }
        }
        self.moving = true;
        self.place_window();
        self.moving = false;
        // The new screen's scale, a re-centering for it, and the app bar's
        // registration there all wait until the move is over.
        self.reregister_appbar = true;
        WindowHandle::new(self.hwnd).post(msg::SETTLE, 0, 0);
    }

    /// Sized for the scale of the screen the notch is on, and centered there.
    /// After a move, also registers the app bar again: the shell tells an app
    /// bar about full-screen apps on the screen it is on.
    fn settle(&mut self) {
        self.sync_scale();
        // Resizes for the scale (whether it changed or not) and centers.
        self.on_display_changed();
        if std::mem::take(&mut self.reregister_appbar) && self.appbar {
            remove_appbar(self.hwnd);
            self.appbar = register_appbar(self.hwnd);
        }
        self.update_fullscreen(true);
    }

    /// Hides the notch while a full-screen app is in front, and brings it back as
    /// soon as it isn't. The shell's word alone isn't enough: it also reports the
    /// desktop as full screen (after Win+D, a click on the wallpaper, or closing
    /// the last window), and it doesn't always say when that is over.
    ///
    /// With no window in front, `settled` decides: just after a switch it is a
    /// passing moment, so wait for the next window; on the timed re-checks it
    /// means nothing is open, which is never full screen.
    fn update_fullscreen(&mut self, settled: bool) {
        let covers = match foreground_is_fullscreen(self.hwnd) {
            Some(covers) => covers,
            None if settled => false,
            None => return,
        };
        // With "Show over full screen" on, a full-screen window is no reason to
        // hide; the notch is put back above it instead.
        let keep = self.settings.show_over_fullscreen;
        if keep && self.shell_fullscreen && covers {
            self.place_window();
        }
        let fullscreen = self.shell_fullscreen && covers && !keep;
        if fullscreen == self.fullscreen {
            return;
        }
        self.fullscreen = fullscreen;
        unsafe {
            if fullscreen {
                let _ = ShowWindow(self.hwnd, SW_HIDE);
            } else {
                let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
                self.place_window();
                // Nothing was drawn while hidden; a countdown may be stale.
                self.render();
            }
        }
    }

    /// Carries out a menu command.
    fn apply_command(&mut self, command: u32) {
        match command {
            // Opened from the message loop: creating a window that takes focus sends
            // messages back here.
            tray::CMD_SETTINGS => WindowHandle::new(self.hwnd).post(msg::SETTINGS_OPEN, 0, 0),
            // Closing from the message loop, not from here, so App isn't freed
            // while this method is still running on it.
            tray::CMD_EXIT => WindowHandle::new(self.hwnd).post(WM_CLOSE, 0, 0),
            _ => {}
        }
    }

    /// Applies what the Settings window saved.
    fn reload_settings(&mut self) {
        let reloaded = Settings::reload(&self.settings);
        let old = std::mem::replace(&mut self.settings, reloaded);
        let new = &self.settings;
        self.fetch_lyrics.store(new.fetch_lyrics, Ordering::Relaxed);
        if new.fetch_lyrics && !old.fetch_lyrics {
            self.media.refresh();
        }
        if new.water_reminders != old.water_reminders {
            // Count the first reminder from now, not from when BUKU started.
            self.water_since = water::unix_now();
        }
        let (focus, rest) = (new.focus_minutes, new.break_minutes);
        self.schedule_water();
        self.pomodoro.set_lengths(focus, rest);
        if self.settings.caps_lock != old.caps_lock || self.settings.caps_custom != old.caps_custom
        {
            self.caps_hook
                .apply(self.settings.caps_lock, &self.settings.caps_custom);
            // Taking the key over turns Caps Lock off; handing it back leaves it as
            // it is. Either way, read it again rather than trust the old flag.
            self.caps_on =
                !capslock::is_taken_over() && unsafe { GetKeyState(VK_CAPITAL.0 as i32) } & 1 != 0;
        }
        if self.settings.screen != old.screen {
            self.move_to_screen();
        }
        accent::set_current(self.settings.accent);
        if self.settings.show_over_fullscreen != old.show_over_fullscreen {
            self.update_fullscreen(true);
        }
        if self.settings.lyrics_early_ms != old.lyrics_early_ms {
            self.schedule_lyric();
        }
        self.retarget();
        if !self.animating {
            self.render();
        }
    }
}

/// The task text box only makes sense on the open Tasks page with room for one more task.
fn task_input_allowed(shown: View, page: Page, full: bool) -> bool {
    shown == View::Card && page == Page::Tasks && !full
}

fn appbar_data(hwnd: HWND) -> APPBARDATA {
    APPBARDATA {
        cbSize: std::mem::size_of::<APPBARDATA>() as u32,
        hWnd: hwnd,
        ..Default::default()
    }
}

/// An app bar that never asks for screen space reserves none; it only gets told
/// when a full-screen app opens or closes on this monitor.
fn register_appbar(hwnd: HWND) -> bool {
    let mut appbar = appbar_data(hwnd);
    appbar.uCallbackMessage = msg::APPBAR;
    unsafe { SHAppBarMessage(ABM_NEW, &mut appbar) != 0 }
}

fn remove_appbar(hwnd: HWND) {
    let mut appbar = appbar_data(hwnd);
    unsafe {
        SHAppBarMessage(ABM_REMOVE, &mut appbar);
    }
}

/// Whether the window in front is a real app covering the whole screen the
/// notch is on: not the desktop or the taskbar, not minimized, not elsewhere,
/// and not just maximized: with an auto-hiding taskbar a maximized window
/// covers the screen too, but it keeps its title bar and its frame hangs a few
/// pixels past each edge, while a full-screen app matches the screen exactly
/// (some Store apps keep the title bar style in full screen). `None` while no
/// window is in front.
fn foreground_is_fullscreen(notch: HWND) -> Option<bool> {
    unsafe {
        let window = GetForegroundWindow();
        if window.is_invalid() {
            return None;
        }
        if IsIconic(window).as_bool() {
            return Some(false);
        }
        let titled = GetWindowLongW(window, GWL_STYLE) as u32 & WS_CAPTION.0 == WS_CAPTION.0;
        let maximized = titled && IsZoomed(window).as_bool();
        let mut class = [0u16; 64];
        let len = GetClassNameW(window, &mut class);
        if is_shell_class(&String::from_utf16_lossy(&class[..len.max(0) as usize])) {
            return Some(false);
        }
        let monitor = MonitorFromWindow(window, MONITOR_DEFAULTTONULL);
        if monitor.is_invalid() || monitor != MonitorFromWindow(notch, MONITOR_DEFAULTTOPRIMARY) {
            return Some(false);
        }
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        let mut rect = RECT::default();
        if !GetMonitorInfoW(monitor, &mut info).as_bool()
            || GetWindowRect(window, &mut rect).is_err()
        {
            return Some(false);
        }
        Some(if maximized {
            rect == info.rcMonitor
        } else {
            covers(&rect, &info.rcMonitor)
        })
    }
}

/// The desktop, its wallpaper layer and the taskbars: the shell sometimes calls
/// these "full screen", but they never hide the notch.
fn is_shell_class(class: &str) -> bool {
    matches!(
        class,
        "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd"
    )
}

fn covers(window: &RECT, monitor: &RECT) -> bool {
    window.left <= monitor.left
        && window.top <= monitor.top
        && window.right >= monitor.right
        && window.bottom >= monitor.bottom
}

/// The notch window, for the foreground hook, which gets no context of its own.
static NOTCH_WINDOW: AtomicIsize = AtomicIsize::new(0);

/// Runs on the UI thread from its message loop (an out-of-context hook), but the
/// App may be borrowed then, so it only posts a message.
unsafe extern "system" fn on_foreground_event(
    _hook: HWINEVENTHOOK,
    _event: u32,
    _window: HWND,
    _object: i32,
    _child: i32,
    _thread: u32,
    _time: u32,
) {
    let notch = NOTCH_WINDOW.load(Ordering::Relaxed);
    if notch != 0 {
        msg::WindowHandle(notch).post(msg::FOREGROUND, 0, 0);
    }
}

impl Drop for App {
    fn drop(&mut self) {
        if !self.foreground_hook.is_invalid() {
            unsafe {
                let _ = UnhookWinEvent(self.foreground_hook);
            }
        }
        NOTCH_WINDOW.store(0, Ordering::Relaxed);
        tray::remove(self.hwnd);
        if self.appbar {
            remove_appbar(self.hwnd);
        }
        if !self.tray_icon.is_invalid() {
            unsafe {
                let _ = DestroyIcon(self.tray_icon);
            }
        }
        for handle in self.power_notify.drain(..) {
            unsafe {
                let _ = UnregisterPowerSettingNotification(handle);
            }
        }
        if self.drop_target.take().is_some() {
            unsafe {
                let _ = RevokeDragDrop(self.hwnd);
            }
        }
        if let Some(token) = self.network_token.take() {
            let _ = NetworkInformation::RemoveNetworkStatusChanged(token);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(left: i32, top: i32, right: i32, bottom: i32) -> RECT {
        RECT {
            left,
            top,
            right,
            bottom,
        }
    }

    #[test]
    fn covers_should_need_the_whole_screen() {
        let screen = rect(0, 0, 2560, 1440);
        assert_eq!(
            [
                covers(&rect(0, 0, 2560, 1440), &screen),
                covers(&rect(-8, -8, 2568, 1448), &screen),
                covers(&rect(-8, -8, 2568, 1400), &screen),
                covers(&rect(100, 0, 2560, 1440), &screen),
            ],
            [true, true, false, false]
        );
    }

    #[test]
    fn the_desktop_and_taskbars_should_never_count_as_full_screen() {
        let classes = [
            "Progman",
            "WorkerW",
            "Shell_TrayWnd",
            "Shell_SecondaryTrayWnd",
            "Chrome_WidgetWin_1",
        ];
        assert_eq!(classes.map(is_shell_class), [true, true, true, true, false]);
    }

    #[test]
    fn task_input_should_open_on_tasks_card_with_room() {
        assert!(task_input_allowed(View::Card, Page::Tasks, false));
    }

    #[test]
    fn task_input_should_not_open_when_list_is_full() {
        assert!(!task_input_allowed(View::Card, Page::Tasks, true));
    }

    #[test]
    fn task_input_should_not_open_without_the_tasks_card() {
        assert!(!task_input_allowed(View::Collapsed, Page::Tasks, false));
        assert!(!task_input_allowed(View::Hud, Page::Tasks, false));
        assert!(!task_input_allowed(View::Card, Page::Shelf, false));
    }
}
