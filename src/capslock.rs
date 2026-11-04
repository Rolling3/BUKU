//! Optional Caps Lock remapping: the shortcut Ctrl+Shift+L, or any key or
//! combination the user records in Settings.
//!
//! This is the one place BUKU intercepts a key, and only when the user picks a
//! mode other than Normal in Settings. A low-level keyboard hook runs on its own
//! thread (so drawing never delays typing). It acts on Caps Lock alone; every
//! other key is passed on untouched at once, and nothing is recorded.

use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU16, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, GetKeyNameTextW, GetKeyState, INPUT, INPUT_0, INPUT_KEYBOARD,
    KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, MAPVK_VK_TO_VSC_EX,
    MapVirtualKeyW, SendInput, VIRTUAL_KEY, VK_CAPITAL, VK_CONTROL, VK_L, VK_LCONTROL, VK_LMENU,
    VK_LSHIFT, VK_LWIN, VK_MENU, VK_PACKET, VK_PROCESSKEY, VK_RCONTROL, VK_RMENU, VK_RSHIFT,
    VK_RWIN, VK_SHIFT, VK_SNAPSHOT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetMessageW, HC_ACTION, KBDLLHOOKSTRUCT, MSG, PM_NOREMOVE, PeekMessageW,
    PostThreadMessageW, SetWindowsHookExW, UnhookWindowsHookEx, WH_KEYBOARD_LL, WM_KEYDOWN,
    WM_QUIT, WM_SYSKEYDOWN, WM_USER,
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapsMode {
    /// Caps Lock does its usual job and no hook is installed.
    #[default]
    Normal,
    /// Sends the shortcut Ctrl+Shift+L once per press.
    CtrlShiftL,
    /// Works as the key or combination recorded in Settings.
    Custom,
}

impl CapsMode {
    pub const ALL: [CapsMode; 3] = [CapsMode::Normal, CapsMode::CtrlShiftL, CapsMode::Custom];

    pub fn label(self) -> &'static str {
        match self {
            CapsMode::Normal => "Normal",
            CapsMode::CtrlShiftL => "Ctrl+Shift+L",
            CapsMode::Custom => "Custom",
        }
    }

    /// The next (`forward`) or previous mode, wrapping around.
    pub fn step(self, forward: bool) -> Self {
        let index = Self::ALL.iter().position(|&mode| mode == self).unwrap_or(0);
        let len = Self::ALL.len();
        Self::ALL[if forward {
            (index + 1) % len
        } else {
            (index + len - 1) % len
        }]
    }

    fn to_u8(self) -> u8 {
        Self::ALL.iter().position(|&mode| mode == self).unwrap_or(0) as u8
    }

    fn from_u8(value: u8) -> Self {
        Self::ALL.get(value as usize).copied().unwrap_or_default()
    }
}

/// Most keys a custom mapping holds: three modifiers and one key.
pub const MAX_CUSTOM_KEYS: usize = 4;

fn is_modifier(key: u16) -> bool {
    [
        VK_LSHIFT,
        VK_RSHIFT,
        VK_LCONTROL,
        VK_RCONTROL,
        VK_LMENU,
        VK_RMENU,
    ]
    .iter()
    .any(|modifier| modifier.0 == key)
}

/// Keys a custom mapping may not use: Caps Lock itself; the Windows keys, whose
/// shortcuts the shell takes before any window sees them; and codes that send
/// nothing as a key: mouse buttons, the IME's stand-in for a key it is
/// composing, and the Unicode packet key.
fn is_refused(key: u16) -> bool {
    key == 0
        || key >= 0xFF
        || (0x01..=0x06).contains(&key)
        || [VK_CAPITAL, VK_LWIN, VK_RWIN, VK_PROCESSKEY, VK_PACKET]
            .iter()
            .any(|k| k.0 == key)
}

/// Whether `keys` (virtual-key codes, modifiers first) can be a custom mapping:
/// one to four keys, none refused, none twice, and any key that is not a
/// modifier last.
pub fn is_valid_custom(keys: &[u16]) -> bool {
    let unique = keys
        .iter()
        .enumerate()
        .all(|(i, key)| !keys[..i].contains(key));
    let modifiers_first = keys
        .iter()
        .take(keys.len().saturating_sub(1))
        .all(|&key| is_modifier(key));
    (1..=MAX_CUSTOM_KEYS).contains(&keys.len())
        && !keys.iter().any(|&key| is_refused(key))
        && unique
        && modifiers_first
}

/// A key's name for Settings: "Ctrl", "Shift", "Alt", "F5", "A", or what the
/// keyboard layout calls it.
fn key_name(key: u16) -> String {
    match VIRTUAL_KEY(key) {
        VK_LCONTROL | VK_RCONTROL => "Ctrl".to_owned(),
        VK_LSHIFT | VK_RSHIFT => "Shift".to_owned(),
        VK_LMENU | VK_RMENU => "Alt".to_owned(),
        // Letters and digits share their ASCII codes, whatever the layout.
        _ if (0x30..=0x39).contains(&key) || (0x41..=0x5A).contains(&key) => {
            char::from(key as u8).to_string()
        }
        // F1 to F24.
        _ if (0x70..=0x87).contains(&key) => format!("F{}", key - 0x6F),
        _ => layout_key_name(key).unwrap_or_else(|| format!("Key {key}")),
    }
}

fn layout_key_name(key: u16) -> Option<String> {
    let scan = unsafe { MapVirtualKeyW(u32::from(key), MAPVK_VK_TO_VSC_EX) };
    if scan == 0 {
        return None;
    }
    // GetKeyNameText takes the scan code in bits 16-23 and the extended flag in bit 24.
    let extended = if scan & 0xFF00 != 0 { 1 << 24 } else { 0 };
    let lparam = (((scan & 0xFF) << 16) | extended) as i32;
    let mut buffer = [0u16; 64];
    let len = unsafe { GetKeyNameTextW(lparam, &mut buffer) };
    let len = usize::try_from(len).ok().filter(|&len| len > 0)?;
    Some(String::from_utf16_lossy(&buffer[..len]))
}

/// A mapping's name for Settings, e.g. "Ctrl+Alt+K".
pub fn keys_label(keys: &[u16]) -> String {
    keys.iter()
        .map(|&key| key_name(key))
        .collect::<Vec<_>>()
        .join("+")
}

/// Records a custom mapping from the keys pressed in the Settings window:
/// modifiers are collected while held, the first other key ends the
/// recording, and releasing a modifier with no other key records the
/// modifiers alone ("Ctrl", "Ctrl+Shift").
#[derive(Debug, Default)]
pub struct Recorder {
    held: Vec<u16>,
}

impl Recorder {
    /// The modifiers held so far, for showing the recording as it goes.
    pub fn held(&self) -> &[u16] {
        &self.held
    }

    /// Takes one key event (a window's virtual-key code, which names Shift,
    /// Ctrl and Alt without their side); returns the mapping once complete.
    pub fn key(&mut self, key: u16, down: bool) -> Option<Vec<u16>> {
        let key = match VIRTUAL_KEY(key) {
            VK_SHIFT => VK_LSHIFT.0,
            VK_CONTROL => VK_LCONTROL.0,
            VK_MENU => VK_LMENU.0,
            _ => key,
        };
        if is_refused(key) {
            return None;
        }
        if !is_modifier(key) {
            // A window only ever gets Print Screen's key-up.
            if !down && key != VK_SNAPSHOT.0 {
                return None;
            }
            let mut keys = std::mem::take(&mut self.held);
            keys.push(key);
            return is_valid_custom(&keys).then_some(keys);
        }
        if down {
            if !self.held.contains(&key) && self.held.len() < MAX_CUSTOM_KEYS - 1 {
                self.held.push(key);
            }
            None
        } else if self.held.is_empty() {
            None
        } else {
            Some(std::mem::take(&mut self.held))
        }
    }
}

/// Up to four virtual-key codes in one word, the first in the low bits, so the
/// hook procedure can read them without a lock.
fn pack(keys: &[u16]) -> u64 {
    keys.iter()
        .take(MAX_CUSTOM_KEYS)
        .enumerate()
        .fold(0, |word, (i, &key)| word | (u64::from(key) << (16 * i)))
}

fn unpack(word: u64) -> Vec<u16> {
    (0..MAX_CUSTOM_KEYS)
        .map(|i| (word >> (16 * i)) as u16)
        .take_while(|&key| key != 0)
        .collect()
}

/// Caps Lock is remapped right now: the hook swallows it and it never toggles.
pub fn is_taken_over() -> bool {
    CapsMode::from_u8(MODE.load(Ordering::Relaxed)) != CapsMode::Normal
}

// The hook procedure is a plain function, so it reads its state from statics.
static MODE: AtomicU8 = AtomicU8::new(0);
static CUSTOM: AtomicU64 = AtomicU64::new(0);
/// Caps Lock is held; its auto-repeat must not send a shortcut again.
static HELD: AtomicBool = AtomicBool::new(false);
/// When Caps Lock last went down (the event's tick count, in ms).
static LAST_DOWN: AtomicU32 = AtomicU32::new(0);

/// Auto-repeat sends key-downs at most the longest repeat delay (one second)
/// apart; a key-down later than this follows a release the hook never saw.
const STALE_HOLD_MS: u32 = 1_500;

/// Whether a Caps Lock key-down starts a press rather than repeating a held key.
/// The release can go missing (it went to the lock screen or an elevated window),
/// and the next press must not be taken for a repeat.
fn is_new_press(held: bool, since_last_down_ms: u32) -> bool {
    !held || since_last_down_ms > STALE_HOLD_MS
}
/// The key BUKU is holding down for a one-key custom mapping (0 when none), so it
/// can be released if the hook stops or the mapping changes before Caps Lock
/// comes up.
static SYNTHETIC: AtomicU16 = AtomicU16::new(0);

/// Marks keys BUKU sends itself, so the hook doesn't act on its own keys or
/// Caps Lock reset. Keys injected by anything else (the on-screen keyboard,
/// automation tools) are remapped like real ones.
const BUKU_KEY: usize = 0x4255_4B55;

/// How often stop() tries to post WM_QUIT (10 ms apart) before giving up.
const QUIT_POST_ATTEMPTS: u32 = 20;

/// Owns the hook thread while a remapping is active.
pub struct CapsHook {
    thread: Option<(u32, JoinHandle<()>)>,
}

impl CapsHook {
    pub fn new() -> Self {
        Self { thread: None }
    }

    /// Applies `mode`; `custom` is the recorded mapping, used in Custom mode. A
    /// Custom mode with nothing valid recorded leaves Caps Lock as it is.
    pub fn apply(&mut self, mode: CapsMode, custom: &[u16]) {
        let mode = match mode {
            CapsMode::Custom if !is_valid_custom(custom) => CapsMode::Normal,
            mode => mode,
        };
        let custom = if mode == CapsMode::Custom {
            pack(custom)
        } else {
            0
        };
        let previous_mode = MODE.swap(mode.to_u8(), Ordering::Relaxed);
        let previous_custom = CUSTOM.swap(custom, Ordering::Relaxed);
        if previous_mode != mode.to_u8() || previous_custom != custom {
            release_synthetic();
        }
        if mode == CapsMode::Normal {
            self.stop();
            return;
        }
        // With the key taken over, a Caps Lock left on would stay on for good.
        if unsafe { GetKeyState(VK_CAPITAL.0 as i32) } & 1 != 0 {
            tap(VK_CAPITAL);
        }
        self.start();
    }

    fn start(&mut self) {
        if self.thread.is_some() {
            return;
        }
        let (ready_tx, ready_rx) = mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("caps-lock".into())
            .spawn(move || unsafe {
                // The queue only exists after the first message call; make it now so
                // stop() can post to this thread as soon as it is reported ready.
                let mut message = MSG::default();
                let _ = PeekMessageW(&mut message, None, WM_USER, WM_USER, PM_NOREMOVE);
                let module = GetModuleHandleW(None).ok();
                let hook =
                    SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook_proc), module.map(Into::into), 0);
                let _ = ready_tx.send(hook.is_ok().then(|| GetCurrentThreadId()));
                let Ok(hook) = hook else { return };
                // Low-level hooks are called through this thread's message loop.
                while GetMessageW(&mut message, None, 0, 0).as_bool() {}
                let _ = UnhookWindowsHookEx(hook);
            });
        if let Ok(handle) = spawned
            && let Ok(Some(thread_id)) = ready_rx.recv()
        {
            self.thread = Some((thread_id, handle));
        }
    }

    fn stop(&mut self) {
        if let Some((thread_id, handle)) = self.thread.take() {
            // The hook goes away with the thread, so no Caps Lock release will follow.
            release_synthetic();
            HELD.store(false, Ordering::Relaxed);
            let posted = (0..QUIT_POST_ATTEMPTS).any(|attempt| {
                if attempt > 0 {
                    std::thread::sleep(Duration::from_millis(10));
                }
                unsafe { PostThreadMessageW(thread_id, WM_QUIT, WPARAM(0), LPARAM(0)) }.is_ok()
            });
            // Never block the UI thread on a thread that can't be told to quit.
            if posted {
                let _ = handle.join();
            }
        }
    }
}

impl Drop for CapsHook {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Keys that share their scan code with a number-pad key and are told apart
/// only by the extended flag, which `MapVirtualKeyW` does not report for them:
/// Page Up to Down arrow, Print Screen, Insert, Delete, the menu key, number
/// pad divide and Num Lock.
fn is_extended(key: u16) -> bool {
    (0x21..=0x28).contains(&key) || [0x2C, 0x2D, 0x2E, 0x5D, 0x6F, 0x90].contains(&key)
}

fn key_input(key: VIRTUAL_KEY, up: bool) -> INPUT {
    // The scan code, and the extended flag for keys that have one (arrows, Home,
    // Insert, right Ctrl...), so programs that read them see the key itself and
    // not its number-pad twin.
    let scan = unsafe { MapVirtualKeyW(u32::from(key.0), MAPVK_VK_TO_VSC_EX) };
    let mut flags = if up {
        KEYEVENTF_KEYUP
    } else {
        KEYBD_EVENT_FLAGS(0)
    };
    if scan & 0xFF00 == 0xE000 || is_extended(key.0) {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: key,
                wScan: (scan & 0xFF) as u16,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: BUKU_KEY,
            },
        },
    }
}

/// Takes the held synthetic key out of `slot`, if any.
fn take_synthetic(slot: &AtomicU16) -> Option<VIRTUAL_KEY> {
    match slot.swap(0, Ordering::Relaxed) {
        0 => None,
        key => Some(VIRTUAL_KEY(key)),
    }
}

/// Lifts a synthetic key that is still down, so it can't stay stuck.
fn release_synthetic() {
    if let Some(key) = take_synthetic(&SYNTHETIC) {
        send(key, true);
    }
}

fn send(key: VIRTUAL_KEY, up: bool) {
    unsafe {
        SendInput(&[key_input(key, up)], std::mem::size_of::<INPUT>() as i32);
    }
}

fn tap(key: VIRTUAL_KEY) {
    unsafe {
        SendInput(
            &[key_input(key, false), key_input(key, true)],
            std::mem::size_of::<INPUT>() as i32,
        );
    }
}

/// Whether a key is down right now, whoever pressed it.
fn is_down(key: u16) -> bool {
    (unsafe { GetAsyncKeyState(i32::from(key)) }) < 0
}

/// `keys` without the ones `held` says are already down: pressing and then
/// releasing a key the user is holding (Ctrl held while Caps Lock is pressed)
/// would lift it under their finger.
fn not_held(keys: &[u16], held: impl Fn(u16) -> bool) -> Vec<u16> {
    keys.iter().copied().filter(|&key| !held(key)).collect()
}

/// Whether a one-key mapping should send `key` down: always when it is the key
/// BUKU already holds (Caps Lock's auto-repeat), never when the user holds it.
fn press_held_key(key: u16, synthetic: u16, held: bool) -> bool {
    synthetic == key || !held
}

/// Presses `keys` in order and releases them in reverse, as one input batch so
/// nothing typed in between can land inside the shortcut. Keys the user is
/// already holding are left alone.
fn shortcut(keys: &[VIRTUAL_KEY]) {
    let codes: Vec<u16> = keys.iter().map(|key| key.0).collect();
    let keys: Vec<VIRTUAL_KEY> = not_held(&codes, is_down)
        .into_iter()
        .map(VIRTUAL_KEY)
        .collect();
    let presses = keys.iter().map(|&key| key_input(key, false));
    let releases = keys.iter().rev().map(|&key| key_input(key, true));
    let inputs: Vec<INPUT> = presses.chain(releases).collect();
    unsafe {
        SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
    }
}

unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        if code == HC_ACTION as i32
            && let Some(event) = (lparam.0 as *const KBDLLHOOKSTRUCT).as_ref()
            && event.vkCode == u32::from(VK_CAPITAL.0)
            && event.dwExtraInfo != BUKU_KEY
        {
            let down = matches!(wparam.0 as u32, WM_KEYDOWN | WM_SYSKEYDOWN);
            let since = if down {
                event
                    .time
                    .wrapping_sub(LAST_DOWN.swap(event.time, Ordering::Relaxed))
            } else {
                0
            };
            let first_down = down && is_new_press(HELD.swap(true, Ordering::Relaxed), since);
            if !down {
                HELD.store(false, Ordering::Relaxed);
            }
            match CapsMode::from_u8(MODE.load(Ordering::Relaxed)) {
                CapsMode::Normal => {}
                CapsMode::CtrlShiftL => {
                    // One shortcut per press; holding the key doesn't repeat it.
                    if first_down {
                        shortcut(&[VK_LCONTROL, VK_LSHIFT, VK_L]);
                    }
                    return LRESULT(1);
                }
                CapsMode::Custom => {
                    let keys = unpack(CUSTOM.load(Ordering::Relaxed));
                    match keys.as_slice() {
                        // One key is held for as long as Caps Lock is, so a modifier
                        // works with other keys and Escape repeats like Escape.
                        [key] => {
                            if down {
                                let synthetic = SYNTHETIC.load(Ordering::Relaxed);
                                if press_held_key(*key, synthetic, is_down(*key)) {
                                    SYNTHETIC.store(*key, Ordering::Relaxed);
                                    send(VIRTUAL_KEY(*key), false);
                                }
                            } else {
                                release_synthetic();
                            }
                        }
                        keys if first_down => {
                            let keys: Vec<VIRTUAL_KEY> =
                                keys.iter().map(|&k| VIRTUAL_KEY(k)).collect();
                            shortcut(&keys);
                        }
                        _ => {}
                    }
                    return LRESULT(1);
                }
            }
        }
        CallNextHookEx(None, code, wparam, lparam)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_press_after_a_lost_release_should_count_as_a_new_press() {
        assert_eq!(
            [
                is_new_press(false, 0),
                // Auto-repeat while held: at most the longest repeat delay apart.
                is_new_press(true, 30),
                is_new_press(true, 1_000),
                // The release went to another desktop; this is a new press.
                is_new_press(true, 5_000),
                // The tick count wrapped between the two presses.
                is_new_press(true, 2u32.wrapping_sub(u32::MAX - 10)),
            ],
            [true, false, false, true, false]
        );
    }

    const A: u16 = 0x41;
    const ESC: u16 = 0x1B;
    const F5: u16 = 0x74;

    #[test]
    fn step_should_wrap_both_ways() {
        assert_eq!(
            (CapsMode::Custom.step(true), CapsMode::Normal.step(false)),
            (CapsMode::Normal, CapsMode::Custom)
        );
    }

    #[test]
    fn take_synthetic_should_return_the_held_key_once() {
        let slot = AtomicU16::new(VK_LCONTROL.0);
        assert_eq!(take_synthetic(&slot), Some(VK_LCONTROL));
        assert_eq!(take_synthetic(&slot), None);
    }

    #[test]
    fn mode_should_round_trip_through_u8() {
        assert!(
            CapsMode::ALL
                .iter()
                .all(|&mode| CapsMode::from_u8(mode.to_u8()) == mode)
        );
    }

    #[test]
    fn a_mode_from_an_older_settings_file_should_not_load() {
        // "off", "language", "escape" and "control" were removed; the settings
        // reader falls back to Normal for a value it can't read.
        for old in ["\"off\"", "\"language\"", "\"escape\"", "\"control\""] {
            assert!(serde_json::from_str::<CapsMode>(old).is_err(), "{old}");
        }
    }

    #[test]
    fn keys_should_round_trip_through_one_word() {
        for keys in [
            vec![ESC],
            vec![VK_LCONTROL.0, VK_LMENU.0, A],
            vec![VK_LCONTROL.0, VK_LSHIFT.0, VK_LMENU.0, F5],
        ] {
            assert_eq!(unpack(pack(&keys)), keys);
        }
    }

    #[test]
    fn a_mapping_should_have_its_one_key_last_and_no_refused_key() {
        let valid = [
            vec![ESC],
            vec![VK_LCONTROL.0],
            vec![VK_LCONTROL.0, VK_LSHIFT.0, A],
        ];
        let invalid = [
            vec![],
            vec![VK_CAPITAL.0],
            vec![VK_LWIN.0, A],
            vec![A, VK_LCONTROL.0],
            vec![A, ESC],
            vec![VK_LCONTROL.0, VK_LCONTROL.0],
            vec![VK_LCONTROL.0, VK_LSHIFT.0, VK_LMENU.0, VK_RCONTROL.0, A],
        ];
        assert!(valid.iter().all(|keys| is_valid_custom(keys)));
        assert!(invalid.iter().all(|keys| !is_valid_custom(keys)));
    }

    #[test]
    fn recording_should_take_the_modifiers_held_then_the_key() {
        let mut recorder = Recorder::default();
        assert_eq!(recorder.key(VK_CONTROL.0, true), None);
        assert_eq!(recorder.key(VK_MENU.0, true), None);
        // Auto-repeat of a held modifier adds nothing.
        assert_eq!(recorder.key(VK_CONTROL.0, true), None);
        assert_eq!(recorder.held(), [VK_LCONTROL.0, VK_LMENU.0]);
        assert_eq!(
            recorder.key(A, true),
            Some(vec![VK_LCONTROL.0, VK_LMENU.0, A])
        );
    }

    #[test]
    fn recording_a_modifier_alone_should_end_when_it_is_released() {
        let mut recorder = Recorder::default();
        assert_eq!(recorder.key(VK_CONTROL.0, true), None);
        assert_eq!(recorder.key(VK_CONTROL.0, false), Some(vec![VK_LCONTROL.0]));
    }

    #[test]
    fn recording_should_ignore_caps_lock_and_the_windows_key() {
        let mut recorder = Recorder::default();
        assert_eq!(recorder.key(VK_CAPITAL.0, true), None);
        assert_eq!(recorder.key(VK_LWIN.0, true), None);
        assert_eq!(recorder.key(ESC, true), Some(vec![ESC]));
    }

    #[test]
    fn keys_that_send_nothing_should_not_be_a_mapping() {
        // Mouse buttons, the IME's stand-in key and the Unicode packet key.
        for key in [0x01, 0x02, 0x04, 0x05, 0x06, 0xE5, 0xE7] {
            assert!(!is_valid_custom(&[key]), "{key:#x}");
        }
        let mut recorder = Recorder::default();
        assert_eq!(recorder.key(0xE5, true), None);
    }

    #[test]
    fn print_screen_should_record_on_its_release() {
        // A window gets only the key-up of Print Screen.
        let mut recorder = Recorder::default();
        assert_eq!(recorder.key(0x2C, false), Some(vec![0x2C]));
    }

    #[test]
    fn navigation_keys_should_be_sent_as_extended_keys() {
        // Without the flag, programs that read scan codes see the number pad.
        let flags = |key: u16| unsafe { key_input(VIRTUAL_KEY(key), false).Anonymous.ki.dwFlags };
        let extended = [0x25, 0x26, 0x24, 0x2D, 0x2E, 0xA3]; // Left, Up, Home, Ins, Del, RCtrl
        assert!(
            extended
                .iter()
                .all(|&key| flags(key).contains(KEYEVENTF_EXTENDEDKEY))
        );
        assert!(
            ![0x41, 0x1B, 0xA2]
                .iter()
                .any(|&key| flags(key).contains(KEYEVENTF_EXTENDEDKEY))
        );
    }

    #[test]
    fn a_held_key_should_be_left_to_the_user() {
        // The user holds left Ctrl: sending its press and release would lift it.
        let ctrl_held = |key: u16| key == VK_LCONTROL.0;
        assert_eq!(
            not_held(&[VK_LCONTROL.0, VK_LSHIFT.0, VK_L.0], ctrl_held),
            [VK_LSHIFT.0, VK_L.0]
        );
        assert_eq!(not_held(&[VK_LCONTROL.0], ctrl_held), Vec::<u16>::new());
        assert_eq!(not_held(&[VK_LSHIFT.0, A], |_| false), [VK_LSHIFT.0, A]);
    }

    #[test]
    fn a_one_key_mapping_should_press_only_a_key_it_does_not_find_held() {
        let ctrl = VK_LCONTROL.0;
        // Fresh press, the user's own Ctrl held, and the auto-repeat of BUKU's own.
        assert!(press_held_key(ctrl, 0, false));
        assert!(!press_held_key(ctrl, 0, true));
        assert!(press_held_key(ctrl, ctrl, true));
    }

    #[test]
    fn a_mapping_should_read_as_its_key_names() {
        assert_eq!(keys_label(&[VK_LCONTROL.0, VK_LMENU.0, A]), "Ctrl+Alt+A");
        assert_eq!(keys_label(&[VK_LSHIFT.0, F5]), "Shift+F5");
    }
}
