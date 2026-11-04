//! The small text box for adding a task.
//!
//! The notch never takes keyboard focus, so typing happens in this separate
//! window. It exists only while the user is adding a task, takes focus for that
//! moment, and closes on Enter (save), Escape or a click elsewhere (cancel).
//! Results go back to the notch as `msg::INPUT_CLOSE`.

use std::sync::OnceLock;

use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DWM_WINDOW_CORNER_PREFERENCE, DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWA_WINDOW_CORNER_PREFERENCE,
    DWMWCP_ROUND, DwmSetWindowAttribute,
};
use windows::Win32::Graphics::Gdi::{
    CLEARTYPE_QUALITY, CreateFontW, CreateSolidBrush, DEFAULT_CHARSET, DeleteObject, FW_NORMAL,
    HBRUSH, HDC, HFONT, OUT_DEFAULT_PRECIS, SetBkColor, SetTextColor,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{SetFocus, VK_ESCAPE, VK_RETURN};
use windows::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, ES_AUTOHSCROLL, GWLP_USERDATA,
    GetWindowLongPtrW, GetWindowTextLengthW, GetWindowTextW, IDC_IBEAM, LoadCursorW,
    RegisterClassExW, SendMessageW, SetForegroundWindow, SetWindowLongPtrW, WA_INACTIVE,
    WINDOW_STYLE, WM_ACTIVATE, WM_CHAR, WM_CTLCOLOREDIT, WM_KEYDOWN, WM_SETFONT, WNDCLASSEXW,
    WS_CHILD, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP, WS_VISIBLE,
};
use windows::core::{Result, w};

use crate::msg::{self, WindowHandle};
use crate::tasks::MAX_TASK_CHARS;

const CLASS: windows::core::PCWSTR = w!("BUKU.TaskInput");
const EM_LIMITTEXT: u32 = 0x00C5;
const EM_SETCUEBANNER: u32 = 0x1501;

const BACKGROUND: COLORREF = rgb(28, 28, 30);
const FOREGROUND: COLORREF = rgb(242, 242, 242);

const fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    COLORREF(r as u32 | (g as u32) << 8 | (b as u32) << 16)
}

/// The background brush lives as long as the window class, i.e. the process.
fn background_brush() -> HBRUSH {
    static BRUSH: OnceLock<isize> = OnceLock::new();
    HBRUSH(*BRUSH.get_or_init(|| unsafe { CreateSolidBrush(BACKGROUND).0 as isize }) as *mut _)
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
            hbrBackground: background_brush(),
            hCursor: LoadCursorW(None, IDC_IBEAM).unwrap_or_default(),
            lpszClassName: CLASS,
            ..Default::default()
        };
        RegisterClassExW(&class) != 0
    })
}

pub struct TaskInput {
    hwnd: HWND,
    edit: HWND,
    font: HFONT,
}

impl TaskInput {
    /// Opens the box at (`x`, `y`) in screen pixels and gives it keyboard focus.
    pub fn open(notch: HWND, x: i32, y: i32, width: i32, height: i32, scale: f32) -> Result<Self> {
        if !register_class() {
            return Err(windows::core::Error::from_thread());
        }
        unsafe {
            let instance = GetModuleHandleW(None)?;
            let hwnd = CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
                CLASS,
                w!("New task"),
                WS_POPUP | WS_VISIBLE,
                x,
                y,
                width,
                height,
                None,
                None,
                Some(instance.into()),
                None,
            )?;
            // Window procedures can't capture state; the notch's handle rides along.
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, notch.0 as isize);
            let round = DWMWCP_ROUND;
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_WINDOW_CORNER_PREFERENCE,
                (&round as *const DWM_WINDOW_CORNER_PREFERENCE).cast(),
                std::mem::size_of_val(&round) as u32,
            );
            let dark = windows::core::BOOL(1);
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_USE_IMMERSIVE_DARK_MODE,
                (&dark as *const windows::core::BOOL).cast(),
                std::mem::size_of_val(&dark) as u32,
            );

            let font_px = (15.0 * scale).round() as i32;
            let pad = (14.0 * scale).round() as i32;
            let edit_h = font_px + (8.0 * scale).round() as i32;
            let edit = match CreateWindowExW(
                Default::default(),
                w!("EDIT"),
                None,
                WS_CHILD | WS_VISIBLE | WINDOW_STYLE(ES_AUTOHSCROLL as u32),
                pad,
                (height - edit_h) / 2,
                width - 2 * pad,
                edit_h,
                Some(hwnd),
                None,
                Some(instance.into()),
                None,
            ) {
                Ok(edit) => edit,
                Err(err) => {
                    let _ = DestroyWindow(hwnd);
                    return Err(err);
                }
            };
            let font = CreateFontW(
                -font_px,
                0,
                0,
                0,
                FW_NORMAL.0 as i32,
                0,
                0,
                0,
                DEFAULT_CHARSET,
                OUT_DEFAULT_PRECIS,
                Default::default(),
                CLEARTYPE_QUALITY,
                0,
                w!("Segoe UI Variable Text"),
            );
            SendMessageW(
                edit,
                WM_SETFONT,
                Some(WPARAM(font.0 as usize)),
                Some(LPARAM(1)),
            );
            SendMessageW(edit, EM_LIMITTEXT, Some(WPARAM(MAX_TASK_CHARS)), None);
            SendMessageW(
                edit,
                EM_SETCUEBANNER,
                Some(WPARAM(1)),
                Some(LPARAM(w!("New task").as_ptr() as isize)),
            );
            let _ = SetWindowSubclass(edit, Some(edit_proc), 1, notch.0 as usize);

            // The click that opened this came to BUKU, so Windows lets it take focus.
            let _ = SetForegroundWindow(hwnd);
            let _ = SetFocus(Some(edit));
            Ok(Self { hwnd, edit, font })
        }
    }

    pub fn text(&self) -> String {
        unsafe {
            let len = GetWindowTextLengthW(self.edit).max(0) as usize;
            let mut buffer = vec![0u16; len + 1];
            let copied = GetWindowTextW(self.edit, &mut buffer).max(0) as usize;
            String::from_utf16_lossy(&buffer[..copied])
        }
    }
}

impl Drop for TaskInput {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyWindow(self.hwnd);
            let _ = DeleteObject(self.font.into());
        }
    }
}

fn notch_of(hwnd: HWND) -> WindowHandle {
    WindowHandle(unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) })
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    unsafe {
        match message {
            WM_CTLCOLOREDIT => {
                let hdc = HDC(wparam.0 as *mut _);
                SetTextColor(hdc, FOREGROUND);
                SetBkColor(hdc, BACKGROUND);
                LRESULT(background_brush().0 as isize)
            }
            // Clicking anywhere else cancels, like a menu.
            WM_ACTIVATE if (wparam.0 & 0xffff) as u32 == WA_INACTIVE => {
                notch_of(hwnd).post(msg::INPUT_CLOSE, 0, 0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, message, wparam, lparam),
        }
    }
}

/// Enter saves and Escape cancels; both are only seen while this box has focus.
unsafe extern "system" fn edit_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    notch: usize,
) -> LRESULT {
    let notch = WindowHandle(notch as isize);
    match message {
        WM_KEYDOWN if wparam.0 == VK_RETURN.0 as usize => {
            notch.post(msg::INPUT_CLOSE, 1, 0);
            LRESULT(0)
        }
        WM_KEYDOWN if wparam.0 == VK_ESCAPE.0 as usize => {
            notch.post(msg::INPUT_CLOSE, 0, 0);
            LRESULT(0)
        }
        // Swallow the characters for those keys too, or the edit control beeps.
        WM_CHAR if wparam.0 == 0x0D || wparam.0 == 0x1B => LRESULT(0),
        _ => unsafe { DefSubclassProc(hwnd, message, wparam, lparam) },
    }
}
