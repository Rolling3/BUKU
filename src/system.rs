//! One-shot reads of system state for the notices: network, keyboard language,
//! battery. Each is called after Windows says something changed, never on a timer.

use windows::Networking::Connectivity::{NetworkConnectivityLevel, NetworkInformation};
use windows::Win32::Globalization::{
    GetLocaleInfoEx, LCIDToLocaleName, LOCALE_SNATIVELANGUAGENAME,
};
use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
use windows::Win32::UI::Input::KeyboardAndMouse::GetKeyboardLayout;
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};
use windows::core::PCWSTR;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NetworkState {
    pub connected: bool,
    pub wifi: bool,
    /// Wi-Fi network or connection name; empty when offline.
    pub name: String,
}

/// The connection Windows uses for the internet right now.
pub fn network() -> NetworkState {
    let offline = NetworkState {
        connected: false,
        wifi: false,
        name: String::new(),
    };
    let Ok(profile) = NetworkInformation::GetInternetConnectionProfile() else {
        return offline;
    };
    let connected = profile
        .GetNetworkConnectivityLevel()
        .is_ok_and(|level| level == NetworkConnectivityLevel::InternetAccess);
    if !connected {
        return offline;
    }
    NetworkState {
        connected,
        wifi: profile.IsWlanConnectionProfile().unwrap_or(false),
        name: profile
            .ProfileName()
            .map(|name| name.to_string())
            .unwrap_or_default(),
    }
}

/// Language id of the keyboard layout the user is typing with, i.e. the one of
/// the window in front.
pub fn keyboard_language() -> u16 {
    unsafe {
        let thread = GetWindowThreadProcessId(GetForegroundWindow(), None);
        (GetKeyboardLayout(thread).0 as usize & 0xffff) as u16
    }
}

/// The language's own name for itself: "English", "العربية".
pub fn language_name(language: u16) -> String {
    let mut locale = [0u16; 85];
    let mut name = [0u16; 128];
    unsafe {
        let len = LCIDToLocaleName(u32::from(language), Some(&mut locale), 0);
        if len <= 1 {
            return String::new();
        }
        let len = GetLocaleInfoEx(
            PCWSTR(locale.as_ptr()),
            LOCALE_SNATIVELANGUAGENAME,
            Some(&mut name),
        );
        let text = String::from_utf16_lossy(&name[..(len.max(1) - 1) as usize]);
        // Some locales give a lowercase native name ("français").
        let mut chars = text.chars();
        match chars.next() {
            Some(first) => first.to_uppercase().chain(chars).collect(),
            None => text,
        }
    }
}

/// Battery charge in percent, or `None` on machines without a battery.
pub fn battery_percent() -> Option<u32> {
    let mut status = SYSTEM_POWER_STATUS::default();
    unsafe { GetSystemPowerStatus(&mut status).ok()? };
    // 128 = no system battery, 255 = unknown.
    let no_battery = status.BatteryFlag == 255 || status.BatteryFlag & 128 != 0;
    (!no_battery && status.BatteryLifePercent <= 100)
        .then_some(u32::from(status.BatteryLifePercent))
}
