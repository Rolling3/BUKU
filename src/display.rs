//! The screens BUKU can sit on, and which one it is set to.
//!
//! A screen is remembered by its Windows device name (`\\.\DISPLAY2`). An empty
//! name means "the main screen", which follows whichever screen Windows calls
//! primary, so a single-screen setup never has to think about it. A remembered
//! screen that is no longer connected falls back to the main one.

use windows::Win32::Foundation::{LPARAM, POINT, RECT};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITOR_DEFAULTTONEAREST, MONITORINFO,
    MONITORINFOEXW, MonitorFromPoint,
};
use windows::Win32::UI::WindowsAndMessaging::MONITORINFOF_PRIMARY;
use windows::core::BOOL;

#[derive(Clone, Debug, PartialEq)]
pub struct Screen {
    /// Windows' device name, such as `\\.\DISPLAY1`.
    pub name: String,
    /// The whole screen, in physical pixels.
    pub rect: RECT,
    pub primary: bool,
}

impl Screen {
    /// The name the settings store for this screen: empty for the main one.
    pub fn setting(&self) -> String {
        if self.primary {
            String::new()
        } else {
            self.name.clone()
        }
    }
}

/// The connected screens: the main one first, then the others left to right.
pub fn screens() -> Vec<Screen> {
    unsafe extern "system" fn collect(
        monitor: HMONITOR,
        _hdc: HDC,
        _rect: *mut RECT,
        list: LPARAM,
    ) -> BOOL {
        let screens = unsafe { &mut *(list.0 as *mut Vec<Screen>) };
        if let Some(screen) = describe(monitor) {
            screens.push(screen);
        }
        true.into()
    }
    let mut screens: Vec<Screen> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(
            None,
            None,
            Some(collect),
            LPARAM(&mut screens as *mut Vec<Screen> as isize),
        );
    }
    order(&mut screens);
    screens
}

fn describe(monitor: HMONITOR) -> Option<Screen> {
    let mut info = MONITORINFOEXW {
        monitorInfo: MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFOEXW>() as u32,
            ..Default::default()
        },
        ..Default::default()
    };
    unsafe {
        GetMonitorInfoW(
            monitor,
            &mut info as *mut MONITORINFOEXW as *mut MONITORINFO,
        )
        .as_bool()
        .then_some(())?;
    }
    let len = info
        .szDevice
        .iter()
        .position(|&c| c == 0)
        .unwrap_or(info.szDevice.len());
    Some(Screen {
        name: String::from_utf16_lossy(&info.szDevice[..len]),
        rect: info.monitorInfo.rcMonitor,
        primary: info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0,
    })
}

/// Main screen first, then left to right, then top to bottom.
fn order(screens: &mut [Screen]) {
    screens.sort_by_key(|screen| (!screen.primary, screen.rect.left, screen.rect.top));
}

/// The screen a setting names, or the main one when it's empty or that screen
/// isn't connected.
pub fn resolve<'a>(screens: &'a [Screen], setting: &str) -> Option<&'a Screen> {
    screens
        .iter()
        .find(|screen| !setting.is_empty() && screen.name.eq_ignore_ascii_case(setting))
        .or_else(|| screens.iter().find(|screen| screen.primary))
        .or_else(|| screens.first())
}

/// The setting for the screen before or after the one `setting` names, wrapping
/// around.
pub fn step(screens: &[Screen], setting: &str, forward: bool) -> String {
    let Some(current) = resolve(screens, setting) else {
        return String::new();
    };
    let index = screens
        .iter()
        .position(|screen| screen == current)
        .unwrap_or(0);
    let count = screens.len();
    let next = if forward {
        (index + 1) % count
    } else {
        (index + count - 1) % count
    };
    screens[next].setting()
}

/// The screen under a point on the desktop (the nearest one if it's in a gap).
pub fn at(point: POINT) -> Option<Screen> {
    describe(unsafe { MonitorFromPoint(point, MONITOR_DEFAULTTONEAREST) })
}

/// What the Settings window calls a screen: "Main screen" or "Screen 2", and
/// its size.
pub fn label(screens: &[Screen], screen: &Screen) -> (String, String) {
    let name = if screen.primary {
        "Main screen".to_owned()
    } else {
        let number = screens.iter().position(|s| s == screen).unwrap_or(0) + 1;
        format!("Screen {number}")
    };
    let size = format!(
        "{} \u{00D7} {}",
        screen.rect.right - screen.rect.left,
        screen.rect.bottom - screen.rect.top
    );
    (name, size)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(name: &str, left: i32, primary: bool) -> Screen {
        Screen {
            name: name.into(),
            rect: RECT {
                left,
                top: 0,
                right: left + 1920,
                bottom: 1080,
            },
            primary,
        }
    }

    fn two_screens() -> Vec<Screen> {
        let mut screens = vec![
            screen(r"\\.\DISPLAY2", -1920, false),
            screen(r"\\.\DISPLAY1", 0, true),
        ];
        order(&mut screens);
        screens
    }

    #[test]
    fn the_main_screen_should_come_first() {
        let names: Vec<String> = two_screens().into_iter().map(|s| s.name).collect();
        assert_eq!(names, [r"\\.\DISPLAY1", r"\\.\DISPLAY2"]);
    }

    #[test]
    fn an_empty_or_missing_setting_should_mean_the_main_screen() {
        let screens = two_screens();
        let picked = ["", r"\\.\DISPLAY9", r"\\.\display2"]
            .map(|setting| resolve(&screens, setting).map(|s| s.name.clone()));
        assert_eq!(
            picked,
            [
                Some(r"\\.\DISPLAY1".to_owned()),
                Some(r"\\.\DISPLAY1".to_owned()),
                Some(r"\\.\DISPLAY2".to_owned()),
            ]
        );
    }

    #[test]
    fn stepping_should_wrap_and_store_the_main_screen_as_empty() {
        let screens = two_screens();
        assert_eq!(
            [
                step(&screens, "", true),
                step(&screens, r"\\.\DISPLAY2", true),
                step(&screens, "", false),
            ],
            [
                r"\\.\DISPLAY2".to_owned(),
                String::new(),
                r"\\.\DISPLAY2".to_owned()
            ]
        );
    }

    #[test]
    fn labels_should_name_the_main_screen_and_number_the_others() {
        let screens = two_screens();
        assert_eq!(
            label(&screens, &screens[1]),
            ("Screen 2".to_owned(), "1920 \u{00D7} 1080".to_owned())
        );
    }
}
