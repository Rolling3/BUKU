//! The Settings window.
//!
//! An ordinary top-level window (it can take focus, unlike the notch), drawn with
//! the same Direct2D setup as the notch so it looks the same in light and dark
//! Windows themes. Every change is saved at once and announced to the notch with
//! `msg::SETTINGS_CHANGED`; there is no Save button.

use std::cell::Cell;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::{D2D_RECT_F, D2D1_COLOR_F};
use windows::Win32::Graphics::Direct2D::{D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_ROUNDED_RECT};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_MEASURING_MODE_NATURAL, DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_LEADING,
    IDWriteTextFormat,
};
use windows::Win32::Graphics::Dwm::{DWMWA_USE_IMMERSIVE_DARK_MODE, DwmSetWindowAttribute};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow,
};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForWindow};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent, VK_ESCAPE,
};
use windows::Win32::UI::Shell::{SEE_MASK_NOASYNC, SHELLEXECUTEINFOW, ShellExecuteExW};
use windows::Win32::UI::WindowsAndMessaging::{
    CREATESTRUCTW, CW_USEDEFAULT, CreateWindowExW, DefWindowProcW, DestroyWindow, GWLP_USERDATA,
    GetWindowLongPtrW, GetWindowRect, IDC_ARROW, IsIconic, LoadCursorW, RegisterClassExW,
    SW_RESTORE, SW_SHOWNORMAL, SWP_NOACTIVATE, SWP_NOZORDER, SetForegroundWindow,
    SetWindowLongPtrW, SetWindowPos, ShowWindow, WA_INACTIVE, WINDOW_EX_STYLE, WM_ACTIVATE,
    WM_CHAR, WM_CLOSE, WM_CREATE, WM_DISPLAYCHANGE, WM_DPICHANGED, WM_EXITSIZEMOVE, WM_KEYDOWN,
    WM_KEYUP, WM_LBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_NCCREATE, WM_NCDESTROY, WM_SYSCHAR,
    WM_SYSKEYDOWN, WM_SYSKEYUP, WNDCLASSEXW, WS_CAPTION, WS_EX_NOREDIRECTIONBITMAP, WS_MINIMIZEBOX,
    WS_OVERLAPPED, WS_SYSMENU,
};
use windows::core::{BOOL, HSTRING, PCWSTR, w};
use windows_numerics::Matrix3x2;

use crate::accent;
use crate::autostart;
use crate::capslock::{self, CapsMode};
use crate::display;
use crate::gfx::{Gfx, rgba};
use crate::msg::{self, WindowHandle};
use crate::settings::{BREAK_MINUTES, FOCUS_MINUTES, LYRICS_EARLY_MS, Settings, WATER_GOAL};
use crate::web_cache;

const CLASS: PCWSTR = w!("BUKU.Settings");
/// Not exported by the Win32 bindings alongside the other mouse messages.
const WM_MOUSELEAVE: u32 = 0x02A3;
/// Least time between attempts to create a graphics device while none can be made.
const GFX_RETRY: Duration = Duration::from_secs(1);

const WIDTH: f32 = 460.0;
const HEIGHT: f32 = 894.0;
const PAD: f32 = 28.0;
const TOP: f32 = 24.0;
const ROW_H: f32 = 46.0;

const BACKGROUND: D2D1_COLOR_F = rgba(0.075, 0.075, 0.08, 1.0);
const SECTION: D2D1_COLOR_F = rgba(1.0, 1.0, 1.0, 0.05);
const TEXT: D2D1_COLOR_F = rgba(1.0, 1.0, 1.0, 0.95);
const TEXT_DIM: D2D1_COLOR_F = rgba(1.0, 1.0, 1.0, 0.55);
const OFF: D2D1_COLOR_F = rgba(1.0, 1.0, 1.0, 0.22);
const HOT: D2D1_COLOR_F = rgba(1.0, 1.0, 1.0, 0.10);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Switch {
    Autostart,
    LyricsInNotch,
    FetchLyrics,
    WaterReminders,
    ShowOverFullscreen,
    AutoHide,
    SpotifyNames,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Number {
    WaterGoal,
    FocusMinutes,
    BreakMinutes,
    LyricsEarly,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Item {
    Switch(Switch),
    Minus(Number),
    Plus(Number),
    /// Previous or next Caps Lock mode.
    CapsStep(bool),
    /// The Caps Lock row's text: in Custom mode, records the key to use.
    CapsRecord,
    /// Previous or next screen for the notch.
    ScreenStep(bool),
    /// Previous or next accent colour.
    AccentStep(bool),
    AppFolder,
    /// Removes any lyrics lookups kept in the Windows web cache.
    ClearCache,
}

enum Row {
    Section(&'static str),
    Switch(Switch, &'static str, &'static str),
    Number(Number, &'static str),
    /// The Caps Lock mode, stepped through with arrows.
    Caps,
    /// The screen the notch sits on, stepped through with arrows.
    Screen,
    /// The accent colour, stepped through with arrows.
    Accent,
}

const ROWS: [Row; 17] = [
    Row::Section("General"),
    Row::Switch(
        Switch::Autostart,
        "Start with Windows",
        "Open BUKU when you sign in",
    ),
    Row::Caps,
    Row::Screen,
    Row::Switch(
        Switch::ShowOverFullscreen,
        "Show over full screen",
        "Stay on top of full-screen videos and games",
    ),
    Row::Switch(
        Switch::AutoHide,
        "Automatically hide BUKU",
        "Show it only when the pointer touches the top",
    ),
    Row::Accent,
    Row::Section("Music"),
    Row::Switch(
        Switch::LyricsInNotch,
        "Lyrics in the notch",
        "Show the current line while music plays",
    ),
    Row::Switch(
        Switch::FetchLyrics,
        "Download lyrics",
        "Look them up on lrclib.net",
    ),
    Row::Number(Number::LyricsEarly, "Show lyrics early by"),
    Row::Switch(
        Switch::SpotifyNames,
        "Name Spotify shortcuts",
        "Ask Spotify for the name, or type one yourself",
    ),
    Row::Section("Focus and water"),
    Row::Number(Number::FocusMinutes, "Focus length"),
    Row::Number(Number::BreakMinutes, "Break length"),
    Row::Switch(
        Switch::WaterReminders,
        "Water reminders",
        "Hourly, 9:00 to 22:00",
    ),
    Row::Number(Number::WaterGoal, "Glasses a day"),
];

/// `value` one step up or down, onto a multiple of the step: a value saved
/// between steps (by an older step size, or by hand) lands on the nearest one
/// in that direction.
fn stepped(number: Number, value: u32, up: bool) -> u32 {
    let (_, _, step) = number.range();
    let below = value / step * step;
    if up {
        below.saturating_add(step)
    } else if below < value {
        below
    } else {
        value.saturating_sub(step)
    }
}

impl Number {
    /// (smallest, largest, step)
    fn range(self) -> (u32, u32, u32) {
        match self {
            Number::WaterGoal => (WATER_GOAL.0, WATER_GOAL.1, 1),
            Number::FocusMinutes => (FOCUS_MINUTES.0, FOCUS_MINUTES.1, 5),
            Number::BreakMinutes => (BREAK_MINUTES.0, BREAK_MINUTES.1, 1),
            Number::LyricsEarly => (LYRICS_EARLY_MS.0, LYRICS_EARLY_MS.1, 100),
        }
    }

    fn get(self, settings: &Settings) -> u32 {
        match self {
            Number::WaterGoal => settings.water_goal,
            Number::FocusMinutes => settings.focus_minutes,
            Number::BreakMinutes => settings.break_minutes,
            Number::LyricsEarly => settings.lyrics_early_ms,
        }
    }

    fn set(self, settings: &mut Settings, value: u32) {
        let (low, high, _) = self.range();
        let value = value.clamp(low, high);
        match self {
            Number::WaterGoal => settings.water_goal = value,
            Number::FocusMinutes => settings.focus_minutes = value,
            Number::BreakMinutes => settings.break_minutes = value,
            Number::LyricsEarly => settings.lyrics_early_ms = value,
        }
    }

    fn label(self, value: u32) -> String {
        match self {
            Number::WaterGoal => value.to_string(),
            Number::FocusMinutes | Number::BreakMinutes => format!("{value} min"),
            // 1200 ms reads "1.2 s", 1000 ms "1 s"; 1250 ms, saved by an older
            // step, still reads "1.25 s".
            Number::LyricsEarly => {
                let seconds = format!("{}.{:02}", value / 1000, value % 1000 / 10);
                format!("{} s", seconds.trim_end_matches('0').trim_end_matches('.'))
            }
        }
    }
}

/// Top of each row, in DIPs.
fn row_tops() -> impl Iterator<Item = (f32, &'static Row)> {
    let mut y = TOP + 40.0;
    ROWS.iter().map(move |row| {
        let top = y;
        y += match row {
            Row::Section(_) => 34.0,
            _ => ROW_H,
        };
        (top, row)
    })
}

const APP_BUTTON: (f32, f32, f32, f32) = (PAD, HEIGHT - 58.0, 150.0, 32.0);
/// Right of "Browse app files"; what it cleared shows at the window's right edge.
const CACHE_BUTTON: (f32, f32, f32, f32) = (PAD + 160.0, HEIGHT - 58.0, 112.0, 32.0);

/// The control under (`x`, `y`), in DIPs.
fn item_at(x: f32, y: f32) -> Option<Item> {
    let (bx, by, bw, bh) = APP_BUTTON;
    if x >= bx && x <= bx + bw && y >= by && y <= by + bh {
        return Some(Item::AppFolder);
    }
    let (bx, by, bw, bh) = CACHE_BUTTON;
    if x >= bx && x <= bx + bw && y >= by && y <= by + bh {
        return Some(Item::ClearCache);
    }
    for (top, row) in row_tops() {
        if y < top || y > top + ROW_H {
            continue;
        }
        match *row {
            // The whole row flips a switch.
            Row::Switch(switch, ..) if (PAD - 10.0..=WIDTH - PAD + 10.0).contains(&x) => {
                return Some(Item::Switch(switch));
            }
            Row::Caps | Row::Screen | Row::Accent => {
                let step = match row {
                    Row::Caps => Item::CapsStep,
                    Row::Screen => Item::ScreenStep,
                    _ => Item::AccentStep,
                };
                let mid = top + ROW_H / 2.0;
                let near = |cx: f32| (x - cx).powi(2) + (y - mid).powi(2) <= 15.0 * 15.0;
                if near(stepper_minus_x()) {
                    return Some(step(false));
                }
                if near(stepper_plus_x()) {
                    return Some(step(true));
                }
                if matches!(row, Row::Caps) && (PAD - 10.0..stepper_minus_x() - 20.0).contains(&x) {
                    return Some(Item::CapsRecord);
                }
            }
            Row::Number(number, _) => {
                let mid = top + ROW_H / 2.0;
                let near = |cx: f32| (x - cx).powi(2) + (y - mid).powi(2) <= 15.0 * 15.0;
                if near(stepper_minus_x()) {
                    return Some(Item::Minus(number));
                }
                if near(stepper_plus_x()) {
                    return Some(Item::Plus(number));
                }
            }
            _ => {}
        }
    }
    None
}

/// The Caps Lock row's second line. `recording` is the modifiers held so far
/// while a key is being recorded.
fn caps_detail(mode: CapsMode, custom: &[u16], recording: Option<&[u16]>) -> String {
    match (mode, recording) {
        (_, Some([])) => "Press the key or keys to use".to_owned(),
        (_, Some(held)) => format!("{}+\u{2026}", capslock::keys_label(held)),
        (CapsMode::Normal, None) => "Works as usual".to_owned(),
        (CapsMode::CtrlShiftL, None) => "Sends the shortcut Ctrl+Shift+L".to_owned(),
        (CapsMode::Custom, None) if capslock::is_valid_custom(custom) => {
            format!(
                "Works as {} \u{00B7} click to change",
                capslock::keys_label(custom)
            )
        }
        (CapsMode::Custom, None) => "Click here, then press a key".to_owned(),
    }
}

fn stepper_minus_x() -> f32 {
    WIDTH - PAD - 120.0
}

/// A row with a title and detail on the left and a value stepped through with
/// arrows on the right; the value is in the accent color when it isn't the default.
fn stepper_row(
    p: &Painter,
    hot: Option<Item>,
    top: f32,
    (title, detail): (&str, &str),
    value: &str,
    changed: bool,
    step: fn(bool) -> Item,
) {
    let fonts = &p.gfx.fonts;
    p.rect(
        PAD - 10.0,
        top + 2.0,
        WIDTH - 2.0 * PAD + 20.0,
        ROW_H - 4.0,
        10.0,
        SECTION,
    );
    p.text(
        title,
        &fonts.body,
        PAD,
        top + 5.0,
        200.0,
        20.0,
        TEXT,
        DWRITE_TEXT_ALIGNMENT_LEADING,
    );
    p.text(
        detail,
        &fonts.small,
        PAD,
        top + 23.0,
        stepper_minus_x() - 13.0 - PAD - 8.0,
        18.0,
        TEXT_DIM,
        DWRITE_TEXT_ALIGNMENT_LEADING,
    );
    let mid = top + ROW_H / 2.0;
    for (cx, glyph, item) in [
        (stepper_minus_x(), "\u{E76B}", step(false)),
        (stepper_plus_x(), "\u{E76C}", step(true)),
    ] {
        let fill = if hot == Some(item) {
            rgba(1.0, 1.0, 1.0, 0.22)
        } else {
            HOT
        };
        p.rect(cx - 13.0, mid - 13.0, 26.0, 26.0, 13.0, fill);
        p.text(
            glyph,
            &fonts.icon_small,
            cx - 13.0,
            mid - 13.0,
            26.0,
            26.0,
            TEXT,
            DWRITE_TEXT_ALIGNMENT_CENTER,
        );
    }
    let label_x = stepper_minus_x() + 13.0;
    let label_w = stepper_plus_x() - 13.0 - label_x;
    p.text(
        value,
        &fonts.body,
        label_x,
        mid - 10.0,
        label_w,
        20.0,
        if changed { accent::current() } else { TEXT },
        DWRITE_TEXT_ALIGNMENT_CENTER,
    );
}

fn stepper_plus_x() -> f32 {
    WIDTH - PAD - 14.0
}

struct State {
    hwnd: HWND,
    notch: WindowHandle,
    gfx: Option<Gfx>,
    scale: f32,
    settings: Settings,
    autostart: bool,
    /// The connected screens, read when the window opens or comes forward.
    screens: Vec<display::Screen>,
    /// A screen just picked here, for `save` to write instead of the file's.
    pending_screen: Option<String>,
    hot: Option<Item>,
    tracking: bool,
    /// Recording the key for Custom Caps Lock mode: the next keys pressed here.
    recorder: Option<capslock::Recorder>,
    /// The keys of a recording just finished, until they are all released: their
    /// auto-repeat and the characters that follow (Escape, Alt+Space) must not
    /// close the window or open its menu.
    settling: Option<Vec<u16>>,
    /// When a graphics device was last tried for, so a missing one isn't retried on every mouse move.
    gfx_tried: Option<Instant>,
    /// The height of the content shown, in DIPs: `HEIGHT`, or less on a short
    /// screen, where the rest scrolls.
    view_h: f32,
    /// How far the content is scrolled, in DIPs.
    scroll: f32,
    /// What the last cache clear removed, shown in place of the version.
    cache_note: Option<String>,
}

/// A cache clear is running. Kept outside the window, which can close and
/// open again while it runs, so a second clear can't start.
static CLEARING: AtomicBool = AtomicBool::new(false);
/// The open Settings window, for a clear that finishes on another thread.
static OPEN_WINDOW: AtomicIsize = AtomicIsize::new(0);

/// Ends the in-flight clear when dropped, even if clearing panics.
struct ClearingDone;

impl Drop for ClearingDone {
    fn drop(&mut self) {
        CLEARING.store(false, Ordering::Release);
    }
}

fn clearing() -> bool {
    CLEARING.load(Ordering::Acquire)
}

thread_local! {
    /// The open Settings window, so a second "Settings…" brings it forward instead.
    static OPEN: Cell<isize> = const { Cell::new(0) };
    /// The state handed to `CreateWindowExW`, until WM_NCCREATE takes it over.
    static PENDING: Cell<*mut State> = const { Cell::new(std::ptr::null_mut()) };
}

fn register_class() -> bool {
    static REGISTERED: OnceLock<bool> = OnceLock::new();
    *REGISTERED.get_or_init(|| unsafe {
        let Ok(instance) = GetModuleHandleW(None) else {
            return false;
        };
        let class = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(window_proc),
            hInstance: instance.into(),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            hIcon: crate::tray::load_icon_sized(32),
            hIconSm: crate::tray::load_icon_sized(16),
            lpszClassName: CLASS,
            ..Default::default()
        };
        RegisterClassExW(&class) != 0
    })
}

/// Opens the Settings window, or brings it forward when it's already open.
pub fn open(notch: HWND) {
    unsafe {
        let open = HWND(OPEN.get() as *mut _);
        if !open.is_invalid() {
            if IsIconic(open).as_bool() {
                let _ = ShowWindow(open, SW_RESTORE);
            }
            let _ = SetForegroundWindow(open);
            return;
        }
        if !register_class() {
            return;
        }
        let Ok(instance) = GetModuleHandleW(None) else {
            return;
        };
        let state = Box::new(State {
            // Filled in by WM_NCCREATE, before any other message.
            hwnd: HWND::default(),
            notch: WindowHandle::new(notch),
            gfx: None,
            scale: 1.0,
            settings: Settings::load(),
            autostart: autostart::is_enabled(),
            screens: display::screens(),
            pending_screen: None,
            hot: None,
            tracking: false,
            recorder: None,
            settling: None,
            gfx_tried: None,
            view_h: HEIGHT,
            scroll: 0.0,
            cache_note: None,
        });
        let raw = Box::into_raw(state);
        PENDING.set(raw);
        let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX;
        let created = CreateWindowExW(
            WS_EX_NOREDIRECTIONBITMAP,
            CLASS,
            w!("BUKU Settings"),
            style,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            None,
            None,
            Some(instance.into()),
            Some(raw.cast()),
        );
        let Ok(hwnd) = created else {
            // Still pending means the window never got as far as WM_NCCREATE, so nothing else owns it.
            let unclaimed = PENDING.replace(std::ptr::null_mut());
            if !unclaimed.is_null() {
                drop(Box::from_raw(unclaimed));
            }
            return;
        };
        OPEN.set(hwnd.0 as isize);
        OPEN_WINDOW.store(hwnd.0 as isize, Ordering::Release);
        let dark = BOOL(1);
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            (&dark as *const BOOL).cast(),
            std::mem::size_of::<BOOL>() as u32,
        );
        fit_to_dpi(hwnd, GetDpiForWindow(hwnd), None);
        let _ = ShowWindow(hwnd, SW_SHOWNORMAL);
        let _ = SetForegroundWindow(hwnd);
        if let Some(state) = state_of(hwnd) {
            state.render();
        }
    }
}

/// Sizes the window so its client area is `WIDTH` x `HEIGHT` DIPs at `dpi`.
/// Shortest the window gets on a small screen, in DIPs; the rest scrolls.
const MIN_VIEW: f32 = 320.0;
/// How far one notch of the mouse wheel scrolls, in DIPs.
const WHEEL_STEP: f32 = 48.0;

/// The content height that fits the screen: `work_px` is the height of the
/// screen's work area and `frame_px` what the title bar and borders take, both in
/// pixels. The whole of `HEIGHT` when it fits.
fn visible_height(work_px: i32, frame_px: i32, scale: f32) -> f32 {
    let room = (work_px - frame_px).max(0) as f32 / scale;
    room.clamp(MIN_VIEW, HEIGHT)
}

/// `scroll` kept between the top and the point where the last row is in view.
fn clamp_scroll(scroll: f32, view_h: f32) -> f32 {
    scroll.clamp(0.0, (HEIGHT - view_h).max(0.0))
}

unsafe fn fit_to_dpi(hwnd: HWND, dpi: u32, suggested: Option<&RECT>) {
    let dpi = clamp_dpi(dpi);
    let scale = dpi as f32 / 96.0;
    let mut frame = RECT {
        left: 0,
        top: 0,
        right: (WIDTH * scale).round() as i32,
        bottom: (HEIGHT * scale).round() as i32,
    };
    unsafe {
        let style = windows::Win32::UI::WindowsAndMessaging::WINDOW_STYLE(
            (WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX).0,
        );
        let _ = AdjustWindowRectExForDpi(&mut frame, style, false, WINDOW_EX_STYLE(0), dpi);
        let frame_px = (frame.bottom - frame.top) - (HEIGHT * scale).round() as i32;
        // On a short screen the window takes the work area's height and the
        // rows scroll; otherwise its bottom rows would be off the screen.
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        let work = GetMonitorInfoW(monitor, &mut info)
            .as_bool()
            .then_some(info.rcWork);
        let view_h = work.map_or(HEIGHT, |work| {
            visible_height(work.bottom - work.top, frame_px, scale)
        });
        let (w, h) = (
            frame.right - frame.left,
            (view_h * scale).round() as i32 + frame_px,
        );
        let mut origin = match suggested {
            Some(r) => (r.left, r.top),
            None => {
                let mut current = RECT::default();
                let _ = GetWindowRect(hwnd, &mut current);
                (current.left, current.top)
            }
        };
        if let Some(work) = work {
            origin.0 = origin.0.min(work.right - w).max(work.left);
            origin.1 = origin.1.min(work.bottom - h).max(work.top);
        }
        let _ = SetWindowPos(
            hwnd,
            None,
            origin.0,
            origin.1,
            w,
            h,
            SWP_NOZORDER | SWP_NOACTIVATE,
        );
        if let Some(state) = state_of(hwnd) {
            state.scale = scale;
            state.view_h = view_h;
            state.scroll = clamp_scroll(state.scroll, view_h);
            let (pw, ph) = (
                (WIDTH * scale).ceil() as u32,
                (view_h * scale).ceil() as u32,
            );
            let resized = state
                .gfx
                .as_mut()
                .map(|gfx| gfx.resize(pw, ph, 96.0 * scale));
            if !matches!(resized, Some(Ok(()))) {
                state.gfx = Gfx::new(hwnd, pw, ph, 96.0 * scale).ok();
            }
        }
    }
}

/// Whether enough time has passed since `last_try` to ask for a graphics device again.
fn retry_due(last_try: Option<Instant>, now: Instant) -> bool {
    last_try.is_none_or(|last| now.saturating_duration_since(last) >= GFX_RETRY)
}

/// A DPI that is safe to size a window with; 0 and absurd values come from broken drivers.
fn clamp_dpi(dpi: u32) -> u32 {
    dpi.clamp(96, 480)
}

fn state_of<'a>(hwnd: HWND) -> Option<&'a mut State> {
    unsafe { (GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut State).as_mut() }
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    unsafe {
        if message == WM_NCCREATE {
            let create = &*(lparam.0 as *const CREATESTRUCTW);
            if let Some(state) = (create.lpCreateParams as *mut State).as_mut() {
                state.hwnd = hwnd;
            }
            PENDING.set(std::ptr::null_mut());
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize);
            return DefWindowProcW(hwnd, message, wparam, lparam);
        }
        // The last message a window gets, also when creation fails after WM_NCCREATE.
        if message == WM_NCDESTROY {
            let state = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut State;
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            if !state.is_null() {
                drop(Box::from_raw(state));
            }
            OPEN.set(0);
            OPEN_WINDOW.store(0, Ordering::Release);
            return LRESULT(0);
        }
        // Handled before borrowing the state: destroying the window frees it. While
        // a key is being recorded, Escape is a key to record, not a way out.
        let recording = state_of(hwnd)
            .is_some_and(|state| state.recorder.is_some() || state.settling.is_some());
        let escape = message == WM_KEYDOWN && wparam.0 == VK_ESCAPE.0 as usize;
        if message == WM_CLOSE || (escape && !recording) {
            let _ = DestroyWindow(hwnd);
            return LRESULT(0);
        }
        let Some(state) = state_of(hwnd) else {
            return DefWindowProcW(hwnd, message, wparam, lparam);
        };
        match message {
            WM_CREATE => LRESULT(0),
            // Every key goes to the recording, Alt and F10 included, so none of
            // them opens the window menu or closes the window.
            WM_KEYDOWN | WM_SYSKEYDOWN | WM_KEYUP | WM_SYSKEYUP | WM_CHAR | WM_SYSCHAR
                if recording =>
            {
                let down = matches!(message, WM_KEYDOWN | WM_SYSKEYDOWN);
                if state.recorder.is_some() && (down || matches!(message, WM_KEYUP | WM_SYSKEYUP)) {
                    state.record_key(wparam.0 as u16, down);
                } else if matches!(message, WM_KEYUP | WM_SYSKEYUP) {
                    state.settle();
                }
                LRESULT(0)
            }
            WM_ACTIVATE if (wparam.0 & 0xffff) as u32 == WA_INACTIVE => {
                // Keys pressed in another window must not end up recorded here.
                state.settling = None;
                if state.recorder.take().is_some() {
                    state.render();
                }
                DefWindowProcW(hwnd, message, wparam, lparam)
            }
            WM_ACTIVATE if (wparam.0 & 0xffff) as u32 != WA_INACTIVE => {
                state.settings = Settings::load();
                state.autostart = autostart::is_enabled();
                state.screens = display::screens();
                state.render();
                DefWindowProcW(hwnd, message, wparam, lparam)
            }
            // A new resolution, or a move to a screen of another height at the same
            // scale (no WM_DPICHANGED then): fit the window to its screen again.
            WM_DISPLAYCHANGE | WM_EXITSIZEMOVE => {
                if message == WM_DISPLAYCHANGE {
                    state.screens = display::screens();
                }
                fit_to_dpi(hwnd, GetDpiForWindow(hwnd), None);
                if let Some(state) = state_of(hwnd) {
                    state.render();
                }
                if message == WM_DISPLAYCHANGE {
                    LRESULT(0)
                } else {
                    DefWindowProcW(hwnd, message, wparam, lparam)
                }
            }
            msg::CACHE_CLEARED => {
                let cleared = web_cache::from_message(wparam.0, lparam.0);
                state.cache_note = Some(web_cache::describe(&cleared));
                state.render();
                LRESULT(0)
            }
            msg::SETTINGS_RELOAD => {
                state.settings = Settings::load();
                state.screens = display::screens();
                state.render();
                LRESULT(0)
            }
            WM_MOUSEWHEEL => {
                let notches = f32::from((wparam.0 >> 16) as u16 as i16) / 120.0;
                let scroll = clamp_scroll(state.scroll - notches * WHEEL_STEP, state.view_h);
                if scroll != state.scroll {
                    state.scroll = scroll;
                    state.render();
                }
                LRESULT(0)
            }
            WM_MOUSEMOVE => {
                if !state.tracking {
                    let mut track = TRACKMOUSEEVENT {
                        cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: hwnd,
                        dwHoverTime: 0,
                    };
                    state.tracking = TrackMouseEvent(&mut track).is_ok();
                }
                let hot = state.item_at(lparam);
                if hot != state.hot {
                    state.hot = hot;
                    state.render();
                }
                LRESULT(0)
            }
            WM_MOUSELEAVE => {
                state.tracking = false;
                if state.hot.take().is_some() {
                    state.render();
                }
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                if let Some(item) = state.item_at(lparam) {
                    state.activate(item);
                    state.render();
                }
                LRESULT(0)
            }
            WM_DPICHANGED => {
                let suggested = (lparam.0 as *const RECT).as_ref();
                fit_to_dpi(hwnd, (wparam.0 & 0xffff) as u32, suggested);
                if let Some(state) = state_of(hwnd) {
                    state.render();
                }
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, message, wparam, lparam),
        }
    }
}

impl State {
    fn item_at(&self, lparam: LPARAM) -> Option<Item> {
        let x = (lparam.0 & 0xffff) as i16 as f32 / self.scale;
        let y = ((lparam.0 >> 16) & 0xffff) as i16 as f32 / self.scale;
        item_at(x, y + self.scroll)
    }

    fn activate(&mut self, item: Item) {
        match item {
            Item::Switch(Switch::Autostart) => {
                let _ = autostart::set_enabled(!self.autostart);
                // Read back what Windows has, in case the change was refused.
                self.autostart = autostart::is_enabled();
            }
            Item::Switch(switch) => {
                let flag = match switch {
                    Switch::LyricsInNotch => &mut self.settings.lyrics_in_notch,
                    Switch::FetchLyrics => &mut self.settings.fetch_lyrics,
                    Switch::WaterReminders => &mut self.settings.water_reminders,
                    Switch::ShowOverFullscreen => &mut self.settings.show_over_fullscreen,
                    Switch::AutoHide => &mut self.settings.auto_hide,
                    Switch::SpotifyNames => &mut self.settings.spotify_names,
                    Switch::Autostart => unreachable!("handled above"),
                };
                *flag = !*flag;
                self.save();
            }
            Item::Minus(number) | Item::Plus(number) => {
                let value = number.get(&self.settings);
                let value = stepped(number, value, matches!(item, Item::Plus(_)));
                number.set(&mut self.settings, value);
                self.save();
            }
            Item::ScreenStep(forward) => {
                // Screens may have come or gone, or the notch may have been dragged,
                // since the window opened.
                self.screens = display::screens();
                let current = Settings::load().screen;
                self.pending_screen = Some(display::step(&self.screens, &current, forward));
                self.save();
            }
            Item::CapsStep(forward) => {
                self.settings.caps_lock = self.settings.caps_lock.step(forward);
                // Custom with nothing recorded yet goes straight to recording.
                let record = self.settings.caps_lock == CapsMode::Custom
                    && !capslock::is_valid_custom(&self.settings.caps_custom);
                self.recorder = record.then(capslock::Recorder::default);
                self.save();
            }
            Item::AccentStep(forward) => {
                self.settings.accent = self.settings.accent.step(forward);
                self.save();
            }
            Item::CapsRecord => {
                // A second click stops a recording without changing anything.
                self.recorder = match self.recorder {
                    None if self.settings.caps_lock == CapsMode::Custom => {
                        Some(capslock::Recorder::default())
                    }
                    _ => None,
                };
            }
            Item::ClearCache => {
                let start =
                    CLEARING.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire);
                if start.is_ok() {
                    self.cache_note = None;
                    std::thread::spawn(|| {
                        let done = ClearingDone;
                        let (wparam, lparam) =
                            web_cache::to_message(&web_cache::clear_lyrics_lookups());
                        drop(done);
                        // The window that is open now, which may not be the one clicked.
                        let open = OPEN_WINDOW.load(Ordering::Acquire);
                        if open != 0 {
                            WindowHandle(open).post(msg::CACHE_CLEARED, wparam, lparam);
                        }
                    });
                }
            }
            Item::AppFolder => {
                // The shell can show UI and pump messages while it opens a folder, which
                // would re-enter this window while its state is borrowed; open it from a
                // short-lived thread instead.
                // The folder BUKU was installed to, where its program files are.
                let exe = std::env::current_exe().ok();
                if let Some(dir) = exe.as_deref().and_then(std::path::Path::parent) {
                    let dir = dir.to_path_buf();
                    std::thread::spawn(move || unsafe {
                        // S_FALSE (already initialised) also needs its CoUninitialize.
                        let initialised = CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_ok();
                        let folder = HSTRING::from(dir.as_os_str());
                        // The shell opens folders asynchronously; without NOASYNC the
                        // request dies with this thread and no window ever appears.
                        let mut info = SHELLEXECUTEINFOW {
                            cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
                            fMask: SEE_MASK_NOASYNC,
                            lpVerb: w!("open"),
                            lpFile: PCWSTR(folder.as_ptr()),
                            nShow: SW_SHOWNORMAL.0,
                            ..Default::default()
                        };
                        let _ = ShellExecuteExW(&mut info);
                        if initialised {
                            CoUninitialize();
                        }
                    });
                }
            }
        }
    }

    /// One key event while recording; a finished recording becomes the Custom
    /// mapping and is saved at once.
    fn record_key(&mut self, key: u16, down: bool) {
        let Some(recorder) = self.recorder.as_mut() else {
            return;
        };
        if let Some(keys) = recorder.key(key, down) {
            self.recorder = None;
            self.settings.caps_lock = CapsMode::Custom;
            self.settings.caps_custom = keys.clone();
            self.settling = Some(keys);
            self.save();
            self.settle();
        }
        self.render();
    }

    /// Ends `settling` once none of the recorded keys is down any more.
    fn settle(&mut self) {
        let down = |key: &u16| unsafe { GetKeyState(i32::from(*key)) } < 0;
        if self
            .settling
            .as_ref()
            .is_some_and(|keys| !keys.iter().any(down))
        {
            self.settling = None;
        }
    }

    fn save(&mut self) {
        // The notch writes the file too (when it is dragged to another screen),
        // so keep the screen the file has rather than the one read at opening.
        if let Some(screen) = self.pending_screen.take() {
            self.settings.screen = screen;
        } else {
            self.settings.screen = Settings::load().screen;
        }
        if self.settings.save().is_ok() {
            self.notch.post(msg::SETTINGS_CHANGED, 0, 0);
        }
    }

    fn render(&mut self) {
        // Drawn in the colour picked here, which the notch reloads on the save.
        accent::set_current(self.settings.accent);
        let now = Instant::now();
        if self.gfx.is_none() && retry_due(self.gfx_tried, now) {
            self.gfx_tried = Some(now);
            // First paint after a failed start, or after the GPU device was lost.
            let (w, h) = (
                (WIDTH * self.scale).ceil() as u32,
                (self.view_h * self.scale).ceil() as u32,
            );
            self.gfx = Gfx::new(self.hwnd, w, h, 96.0 * self.scale).ok();
        }
        let Some(gfx) = &self.gfx else { return };
        gfx.begin();
        unsafe {
            gfx.ctx
                .SetTransform(&Matrix3x2::translation(0.0, -self.scroll));
        }
        let p = Painter { gfx };
        p.rect(0.0, 0.0, WIDTH, HEIGHT, 0.0, BACKGROUND);
        p.text(
            "Settings",
            &gfx.fonts.big,
            PAD,
            TOP - 4.0,
            WIDTH - 2.0 * PAD,
            44.0,
            TEXT,
            DWRITE_TEXT_ALIGNMENT_LEADING,
        );

        for (top, row) in row_tops() {
            match *row {
                Row::Section(title) => {
                    p.text(
                        title,
                        &gfx.fonts.small,
                        PAD,
                        top + 12.0,
                        WIDTH,
                        18.0,
                        TEXT_DIM,
                        DWRITE_TEXT_ALIGNMENT_LEADING,
                    );
                }
                Row::Switch(switch, title, detail) => {
                    if self.hot == Some(Item::Switch(switch)) {
                        p.rect(
                            PAD - 10.0,
                            top + 2.0,
                            WIDTH - 2.0 * PAD + 20.0,
                            ROW_H - 4.0,
                            10.0,
                            HOT,
                        );
                    } else {
                        p.rect(
                            PAD - 10.0,
                            top + 2.0,
                            WIDTH - 2.0 * PAD + 20.0,
                            ROW_H - 4.0,
                            10.0,
                            SECTION,
                        );
                    }
                    p.text(
                        title,
                        &gfx.fonts.body,
                        PAD,
                        top + 5.0,
                        WIDTH - 2.0 * PAD - 60.0,
                        20.0,
                        TEXT,
                        DWRITE_TEXT_ALIGNMENT_LEADING,
                    );
                    p.text(
                        detail,
                        &gfx.fonts.small,
                        PAD,
                        top + 23.0,
                        WIDTH - 2.0 * PAD - 60.0,
                        18.0,
                        TEXT_DIM,
                        DWRITE_TEXT_ALIGNMENT_LEADING,
                    );
                    let on = match switch {
                        Switch::Autostart => self.autostart,
                        Switch::LyricsInNotch => self.settings.lyrics_in_notch,
                        Switch::FetchLyrics => self.settings.fetch_lyrics,
                        Switch::WaterReminders => self.settings.water_reminders,
                        Switch::ShowOverFullscreen => self.settings.show_over_fullscreen,
                        Switch::AutoHide => self.settings.auto_hide,
                        Switch::SpotifyNames => self.settings.spotify_names,
                    };
                    p.switch(WIDTH - PAD - 44.0, top + ROW_H / 2.0 - 11.0, on);
                }
                Row::Caps => {
                    let detail = caps_detail(
                        self.settings.caps_lock,
                        &self.settings.caps_custom,
                        self.recorder.as_ref().map(capslock::Recorder::held),
                    );
                    stepper_row(
                        &p,
                        self.hot,
                        top,
                        ("Caps Lock key", &detail),
                        self.settings.caps_lock.label(),
                        self.settings.caps_lock != CapsMode::Normal,
                        Item::CapsStep,
                    );
                }
                Row::Accent => {
                    stepper_row(
                        &p,
                        self.hot,
                        top,
                        ("Accent colour", "Switches, music bars and highlights"),
                        self.settings.accent.label(),
                        true,
                        Item::AccentStep,
                    );
                }
                Row::Screen => {
                    let current = display::resolve(&self.screens, &self.settings.screen);
                    let (name, size) = current
                        .map(|screen| display::label(&self.screens, screen))
                        .unwrap_or_else(|| ("Main screen".to_owned(), String::new()));
                    let detail = if self.screens.len() > 1 {
                        format!("{size} \u{00B7} or drag the notch there")
                    } else {
                        "Connect another screen to move the notch".to_owned()
                    };
                    stepper_row(
                        &p,
                        self.hot,
                        top,
                        ("Notch screen", &detail),
                        &name,
                        current.is_some_and(|screen| !screen.primary),
                        Item::ScreenStep,
                    );
                }
                Row::Number(number, title) => {
                    p.rect(
                        PAD - 10.0,
                        top + 2.0,
                        WIDTH - 2.0 * PAD + 20.0,
                        ROW_H - 4.0,
                        10.0,
                        SECTION,
                    );
                    p.text(
                        title,
                        &gfx.fonts.body,
                        PAD,
                        top + 13.0,
                        200.0,
                        20.0,
                        TEXT,
                        DWRITE_TEXT_ALIGNMENT_LEADING,
                    );
                    let mid = top + ROW_H / 2.0;
                    let value = number.get(&self.settings);
                    let (low, high, _) = number.range();
                    for (cx, glyph, item, enabled) in [
                        (
                            stepper_minus_x(),
                            "\u{E738}",
                            Item::Minus(number),
                            value > low,
                        ),
                        (
                            stepper_plus_x(),
                            "\u{E710}",
                            Item::Plus(number),
                            value < high,
                        ),
                    ] {
                        let fill = if self.hot == Some(item) && enabled {
                            rgba(1.0, 1.0, 1.0, 0.22)
                        } else {
                            HOT
                        };
                        p.rect(cx - 13.0, mid - 13.0, 26.0, 26.0, 13.0, fill);
                        p.text(
                            glyph,
                            &gfx.fonts.icon_small,
                            cx - 13.0,
                            mid - 13.0,
                            26.0,
                            26.0,
                            if enabled { TEXT } else { TEXT_DIM },
                            DWRITE_TEXT_ALIGNMENT_CENTER,
                        );
                    }
                    let label_x = stepper_minus_x() + 13.0;
                    let label_w = stepper_plus_x() - 13.0 - label_x;
                    p.text(
                        &number.label(value),
                        &gfx.fonts.body,
                        label_x,
                        mid - 10.0,
                        label_w,
                        20.0,
                        TEXT,
                        DWRITE_TEXT_ALIGNMENT_CENTER,
                    );
                }
            }
        }

        let (bx, by, bw, bh) = APP_BUTTON;
        let fill = if self.hot == Some(Item::AppFolder) {
            rgba(1.0, 1.0, 1.0, 0.18)
        } else {
            HOT
        };
        p.rect(bx, by, bw, bh, bh / 2.0, fill);
        p.text(
            "Browse app files",
            &gfx.fonts.small,
            bx,
            by,
            bw,
            bh,
            TEXT,
            DWRITE_TEXT_ALIGNMENT_CENTER,
        );
        let (cx, cy, cw, ch) = CACHE_BUTTON;
        let fill = if self.hot == Some(Item::ClearCache) && !clearing() {
            rgba(1.0, 1.0, 1.0, 0.18)
        } else {
            HOT
        };
        p.rect(cx, cy, cw, ch, ch / 2.0, fill);
        p.text(
            if clearing() {
                "Clearing..."
            } else {
                "Clear cache"
            },
            &gfx.fonts.small,
            cx,
            cy,
            cw,
            ch,
            if clearing() { TEXT_DIM } else { TEXT },
            DWRITE_TEXT_ALIGNMENT_CENTER,
        );
        let version = concat!("BUKU ", env!("CARGO_PKG_VERSION"));
        let (note, color) = match &self.cache_note {
            Some(note) => (note.as_str(), TEXT),
            None => (version, TEXT_DIM),
        };
        let note_x = cx + cw + 8.0;
        p.text(
            note,
            &gfx.fonts.small,
            note_x,
            by - 6.0,
            WIDTH - PAD - note_x,
            bh + 12.0,
            color,
            windows::Win32::Graphics::DirectWrite::DWRITE_TEXT_ALIGNMENT_TRAILING,
        );

        if gfx.end().is_err() {
            // Device lost; the next render rebuilds it.
            self.gfx = None;
        }
    }
}

struct Painter<'a> {
    gfx: &'a Gfx,
}

impl Painter<'_> {
    fn rect(&self, x: f32, y: f32, w: f32, h: f32, r: f32, color: D2D1_COLOR_F) {
        unsafe {
            self.gfx.brush.SetColor(&color);
            self.gfx.ctx.FillRoundedRectangle(
                &D2D1_ROUNDED_RECT {
                    rect: D2D_RECT_F {
                        left: x,
                        top: y,
                        right: x + w,
                        bottom: y + h,
                    },
                    radiusX: r,
                    radiusY: r,
                },
                &self.gfx.brush,
            );
        }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "text, style, box, color and alignment"
    )]
    fn text(
        &self,
        text: &str,
        format: &IDWriteTextFormat,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        color: D2D1_COLOR_F,
        align: windows::Win32::Graphics::DirectWrite::DWRITE_TEXT_ALIGNMENT,
    ) {
        let wide: Vec<u16> = text.encode_utf16().collect();
        unsafe {
            let _ = format.SetTextAlignment(align);
            self.gfx.brush.SetColor(&color);
            self.gfx.ctx.DrawText(
                &wide,
                format,
                &D2D_RECT_F {
                    left: x,
                    top: y,
                    right: x + w,
                    bottom: y + h,
                },
                &self.gfx.brush,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
                DWRITE_MEASURING_MODE_NATURAL,
            );
            let _ = format.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_LEADING);
        }
    }

    fn switch(&self, x: f32, y: f32, on: bool) {
        self.rect(
            x,
            y,
            44.0,
            22.0,
            11.0,
            if on { accent::current() } else { OFF },
        );
        let knob_x = if on { x + 24.0 } else { x + 2.0 };
        self.rect(
            knob_x,
            y + 2.0,
            18.0,
            18.0,
            9.0,
            if on { BACKGROUND } else { TEXT },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn number_set_should_clamp_to_range() {
        let mut settings = Settings::default();
        Number::FocusMinutes.set(&mut settings, 500);
        Number::BreakMinutes.set(&mut settings, 0);
        assert_eq!((settings.focus_minutes, settings.break_minutes), (180, 1));
    }

    #[test]
    fn clamp_dpi_should_keep_the_dpi_in_a_sane_range() {
        assert_eq!(
            (clamp_dpi(0), clamp_dpi(144), clamp_dpi(100_000)),
            (96, 144, 480)
        );
    }

    #[test]
    fn retry_due_should_wait_a_second_between_attempts() {
        let start = Instant::now();
        assert!(retry_due(None, start));
        assert!(!retry_due(Some(start), start + Duration::from_millis(300)));
        assert!(retry_due(Some(start), start + GFX_RETRY));
    }

    #[test]
    fn the_lyrics_lead_should_read_in_seconds() {
        assert_eq!(
            [0, 100, 300, 500, 1_000, 1_200, 1_250, 3_000].map(|ms| Number::LyricsEarly.label(ms)),
            [
                "0 s", "0.1 s", "0.3 s", "0.5 s", "1 s", "1.2 s", "1.25 s", "3 s"
            ]
        );
    }

    #[test]
    fn the_lyrics_lead_should_move_a_tenth_of_a_second_at_a_time() {
        let early = Number::LyricsEarly;
        assert_eq!(
            [
                stepped(early, 0, true),
                stepped(early, 100, true),
                stepped(early, 1_000, true),
                stepped(early, 1_000, false),
                stepped(early, 100, false),
                stepped(early, 0, false),
            ],
            [100, 200, 1_100, 900, 0, 0]
        );
    }

    #[test]
    fn a_lead_saved_off_the_grid_should_snap_onto_it() {
        // 1.25 s was a step before the steps became a tenth of a second.
        let early = Number::LyricsEarly;
        assert_eq!(
            [stepped(early, 1_250, true), stepped(early, 1_250, false)],
            [1_300, 1_200]
        );
    }

    #[test]
    fn other_numbers_should_keep_their_steps() {
        assert_eq!(
            [
                stepped(Number::FocusMinutes, 25, true),
                stepped(Number::BreakMinutes, 5, false),
                stepped(Number::WaterGoal, 8, true),
            ],
            [30, 4, 9]
        );
    }

    #[test]
    fn the_last_row_should_end_above_the_app_folder_button() {
        let (top, _) = row_tops().last().unwrap_or((0.0, &ROWS[0]));
        assert!(top + ROW_H <= APP_BUTTON.1);
    }

    #[test]
    fn the_window_should_fit_a_short_screen_and_scroll_the_rest() {
        // 1920x1080 at 100%: everything fits. 1366x768 at 100%: the rows scroll.
        // A tiny work area still leaves a usable window.
        assert_eq!(visible_height(1032, 31, 1.0), HEIGHT);
        assert_eq!(visible_height(728, 31, 1.0), 697.0);
        assert_eq!(visible_height(200, 31, 1.0), MIN_VIEW);
        // 1920x1080 at 125%: 1032 px of work area is 825.6 DIPs, minus the frame.
        assert!(visible_height(1032, 39, 1.25) < HEIGHT);
    }

    #[test]
    fn scrolling_should_stop_at_the_top_and_at_the_last_row() {
        assert_eq!(clamp_scroll(-30.0, 697.0), 0.0);
        assert_eq!(clamp_scroll(1_000.0, 697.0), HEIGHT - 697.0);
        assert_eq!(clamp_scroll(50.0, HEIGHT), 0.0);
    }

    #[test]
    fn the_caps_row_should_say_what_the_key_does() {
        let ctrl_alt_k = [0xA2, 0xA4, 0x4B];
        assert_eq!(
            [
                caps_detail(CapsMode::Normal, &[], None),
                caps_detail(CapsMode::CtrlShiftL, &[], None),
                caps_detail(CapsMode::Custom, &ctrl_alt_k, None),
                caps_detail(CapsMode::Custom, &[], None),
                caps_detail(CapsMode::Custom, &ctrl_alt_k, Some(&[])),
                caps_detail(CapsMode::Custom, &[], Some(&[0xA2])),
            ],
            [
                "Works as usual",
                "Sends the shortcut Ctrl+Shift+L",
                "Works as Ctrl+Alt+K \u{00B7} click to change",
                "Click here, then press a key",
                "Press the key or keys to use",
                "Ctrl+\u{2026}",
            ]
        );
    }

    #[test]
    fn clicking_the_caps_row_text_should_record_and_its_arrows_should_step() {
        let (top, _) = row_tops()
            .find(|(_, row)| matches!(row, Row::Caps))
            .unwrap_or((0.0, &ROWS[0]));
        let mid = top + ROW_H / 2.0;
        assert_eq!(
            [PAD + 40.0, stepper_minus_x(), stepper_plus_x()].map(|x| item_at(x, mid)),
            [
                Some(Item::CapsRecord),
                Some(Item::CapsStep(false)),
                Some(Item::CapsStep(true))
            ]
        );
    }

    #[test]
    fn item_at_should_find_the_app_folder_button() {
        let (x, y, w, h) = APP_BUTTON;
        assert_eq!(item_at(x + w / 2.0, y + h / 2.0), Some(Item::AppFolder));
    }

    #[test]
    fn the_clear_cache_button_should_sit_right_of_browse_app_files() {
        let (ax, _, aw, _) = APP_BUTTON;
        let (x, y, w, h) = CACHE_BUTTON;
        assert!(x > ax + aw && x + w < WIDTH - PAD);
        assert_eq!(
            [
                item_at(x + w / 2.0, y + h / 2.0),
                item_at(ax + aw + 4.0, y + h / 2.0)
            ],
            [Some(Item::ClearCache), None]
        );
    }
}

#[cfg(test)]
mod edge_case_tests {
    use super::*;

    #[test]
    fn a_switch_row_should_respond_across_its_whole_drawn_width() {
        let top = row_tops()
            .find(|(_, row)| matches!(row, Row::Switch(Switch::Autostart, ..)))
            .map(|(top, _)| top)
            .unwrap_or_default();
        let mid = top + ROW_H / 2.0;
        assert_eq!(
            (item_at(PAD - 6.0, mid), item_at(WIDTH - PAD + 6.0, mid)),
            (
                Some(Item::Switch(Switch::Autostart)),
                Some(Item::Switch(Switch::Autostart))
            )
        );
    }

    #[test]
    fn plus_should_never_lower_a_value_that_settings_accept() {
        let mut settings = Settings {
            focus_minutes: 180,
            break_minutes: 60,
            ..Settings::default()
        };
        for (number, value) in [(Number::FocusMinutes, 180), (Number::BreakMinutes, 60)] {
            let (_, _, step) = number.range();
            number.set(&mut settings, value + step);
            assert!(
                number.get(&settings) >= value,
                "{number:?} dropped below {value}"
            );
        }
    }
}

#[cfg(test)]
mod prop_tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn a_lead_label_should_read_back_as_its_value(tenths in 0u32..=300) {
            let ms = tenths * 10;
            let label = Number::LyricsEarly.label(ms);
            let seconds: f64 = label.trim_end_matches(" s").parse().unwrap_or(-1.0);
            prop_assert_eq!((seconds * 1000.0).round() as u32, ms, "{}", label);
        }

        #[test]
        fn saved_settings_should_load_back(early in 0u32..=3_000, water in 1u32..=30, focus in 1u32..=180, rest in 1u32..=60, lyrics in any::<bool>()) {
            let settings = Settings {
                lyrics_in_notch: lyrics,
                lyrics_early_ms: early,
                water_goal: water,
                focus_minutes: focus,
                break_minutes: rest,
                ..Settings::default()
            };
            let json = serde_json::to_string(&settings).unwrap_or_default();
            let loaded: Settings = serde_json::from_str(&json).unwrap_or_default();
            prop_assert_eq!(serde_json::to_string(&loaded).unwrap_or_default(), json);
        }
    }
}
