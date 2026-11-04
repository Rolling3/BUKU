//! User settings, stored as JSON in `%APPDATA%\BUKU\settings.json`.

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use crate::accent::Accent;
use crate::capslock::{self, CapsMode};
use crate::store;

const FILE: &str = "settings.json";

// Allowed ranges, shared by loading and the Settings window so a stepper never
// pulls a value the file accepts back the wrong way.
pub const WATER_GOAL: (u32, u32) = (1, 30);
pub const FOCUS_MINUTES: (u32, u32) = (1, 180);
pub const BREAK_MINUTES: (u32, u32) = (1, 60);
pub const LYRICS_EARLY_MS: (u32, u32) = (0, 3_000);

#[derive(Clone, Debug, Serialize)]
pub struct Settings {
    /// Show the current lyric line inside the collapsed notch while music plays.
    pub lyrics_in_notch: bool,
    /// Fetch lyrics from lrclib.net. Off means BUKU makes no network requests at all.
    pub fetch_lyrics: bool,
    /// How long before its time each lyric line shows.
    pub lyrics_early_ms: u32,
    /// Nudge to drink water when none was logged for a while during the day.
    pub water_reminders: bool,
    pub water_goal: u32,
    pub focus_minutes: u32,
    pub break_minutes: u32,
    /// What the Caps Lock key does.
    pub caps_lock: CapsMode,
    /// The key or keys Caps Lock works as in Custom mode: virtual-key codes,
    /// modifiers first; empty until recorded.
    pub caps_custom: Vec<u16>,
    /// The screen the notch sits on, by Windows' device name (`\\.\DISPLAY2`);
    /// empty for the main screen. See `display`.
    pub screen: String,
    /// Keep the notch on screen over full-screen videos and games instead of
    /// hiding it.
    pub show_over_fullscreen: bool,
    /// The highlight colour of the notch and Settings.
    pub accent: Accent,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            lyrics_in_notch: true,
            fetch_lyrics: true,
            lyrics_early_ms: 1_000,
            water_reminders: true,
            water_goal: 8,
            focus_minutes: 25,
            break_minutes: 5,
            caps_lock: CapsMode::Normal,
            caps_custom: Vec::new(),
            screen: String::new(),
            show_over_fullscreen: false,
            accent: Accent::White,
        }
    }
}

/// Read field by field: a value that can't be used (an option from a newer
/// version, a typo, a number out of range) falls back to that field's default
/// instead of throwing away the whole file, which the next save would overwrite.
impl<'de> Deserialize<'de> for Settings {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        let field = |name: &str| value.get(name);
        let flag =
            |name: &str, default: bool| field(name).and_then(Value::as_bool).unwrap_or(default);
        let defaults = Self::default();
        Ok(Self {
            lyrics_in_notch: flag("lyrics_in_notch", defaults.lyrics_in_notch),
            fetch_lyrics: flag("fetch_lyrics", defaults.fetch_lyrics),
            lyrics_early_ms: whole_number(field("lyrics_early_ms"))
                .unwrap_or(defaults.lyrics_early_ms),
            water_reminders: flag("water_reminders", defaults.water_reminders),
            water_goal: whole_number(field("water_goal")).unwrap_or(defaults.water_goal),
            focus_minutes: whole_number(field("focus_minutes")).unwrap_or(defaults.focus_minutes),
            break_minutes: whole_number(field("break_minutes")).unwrap_or(defaults.break_minutes),
            caps_lock: field("caps_lock")
                .and_then(|mode| serde_json::from_value(mode.clone()).ok())
                .unwrap_or(defaults.caps_lock),
            caps_custom: field("caps_custom")
                .and_then(|keys| serde_json::from_value::<Vec<u16>>(keys.clone()).ok())
                .filter(|keys| capslock::is_valid_custom(keys))
                .unwrap_or(defaults.caps_custom),
            // Device names are short; anything long is not one.
            screen: field("screen")
                .and_then(Value::as_str)
                .filter(|name| name.len() <= 64)
                .map_or(defaults.screen, str::to_owned),
            show_over_fullscreen: flag("show_over_fullscreen", defaults.show_over_fullscreen),
            accent: field("accent")
                .and_then(|accent| serde_json::from_value(accent.clone()).ok())
                .unwrap_or(defaults.accent),
        })
    }
}

/// A non-negative number, saturated to `u32` (callers clamp to their own range).
fn whole_number(value: Option<&Value>) -> Option<u32> {
    let value = value?;
    let number = match value.as_u64() {
        Some(n) => n,
        None => value
            .as_f64()
            .filter(|f| f.is_finite() && *f >= 0.0)?
            .round() as u64,
    };
    Some(number.min(u64::from(u32::MAX)) as u32)
}

impl Settings {
    /// Loads the settings, pulling hand-edited numbers back into a sane range.
    pub fn load() -> Self {
        let mut settings: Self = store::load(FILE);
        settings.water_goal = settings.water_goal.clamp(WATER_GOAL.0, WATER_GOAL.1);
        settings.focus_minutes = settings
            .focus_minutes
            .clamp(FOCUS_MINUTES.0, FOCUS_MINUTES.1);
        settings.break_minutes = settings
            .break_minutes
            .clamp(BREAK_MINUTES.0, BREAK_MINUTES.1);
        settings.lyrics_early_ms = settings
            .lyrics_early_ms
            .clamp(LYRICS_EARLY_MS.0, LYRICS_EARLY_MS.1);
        settings
    }

    pub fn save(&self) -> std::io::Result<()> {
        store::save(FILE, self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CUSTOM: &str = r#""lyrics_in_notch":false,"fetch_lyrics":false,"water_reminders":false,"water_goal":12,"focus_minutes":50,"break_minutes":10"#;

    fn custom(settings: &Settings) -> bool {
        !settings.lyrics_in_notch
            && !settings.fetch_lyrics
            && !settings.water_reminders
            && settings.focus_minutes == 50
            && settings.break_minutes == 10
    }

    #[test]
    fn unknown_caps_lock_value_should_keep_the_other_settings() {
        let json = format!(r#"{{{CUSTOM},"caps_lock":"hyper"}}"#);
        let settings: Option<Settings> = serde_json::from_str(&json).ok();
        assert!(
            settings.is_some_and(|s| custom(&s)
                && s.water_goal == 12
                && s.caps_lock == CapsMode::Normal)
        );
    }

    #[test]
    fn oversized_number_should_keep_the_other_settings() {
        let json =
            format!(r#"{{{CUSTOM},"water_goal":4294967296}}"#).replace(r#""water_goal":12,"#, "");
        let settings: Option<Settings> = serde_json::from_str(&json).ok();
        assert!(settings.is_some_and(|s| custom(&s)));
    }

    #[test]
    fn a_custom_caps_key_that_cannot_be_used_should_mean_none() {
        // Caps Lock itself, a key after the main key, text, and a number too large.
        let mappings = [r"[20]", r"[65, 162]", r#""ctrl""#, r"[70000]"].map(|keys| {
            serde_json::from_str::<Settings>(&format!(r#"{{{CUSTOM},"caps_custom":{keys}}}"#))
                .map(|s| (custom(&s), s.caps_custom))
                .ok()
        });
        assert_eq!(
            mappings,
            [
                Some((true, Vec::new())),
                Some((true, Vec::new())),
                Some((true, Vec::new())),
                Some((true, Vec::new()))
            ]
        );
    }

    #[test]
    fn a_screen_name_that_is_not_text_or_too_long_should_mean_the_main_screen() {
        let long = "x".repeat(65);
        let screens = [
            r#"{"screen":7}"#.to_owned(),
            format!(r#"{{"screen":"{long}"}}"#),
        ]
        .map(|json| {
            serde_json::from_str::<Settings>(&json)
                .map(|s| s.screen)
                .unwrap_or_default()
        });
        assert_eq!(screens, [String::new(), String::new()]);
    }

    #[test]
    fn every_field_should_survive_a_save_and_load() {
        let saved = Settings {
            lyrics_in_notch: false,
            fetch_lyrics: false,
            lyrics_early_ms: 250,
            water_reminders: false,
            water_goal: 3,
            focus_minutes: 40,
            break_minutes: 7,
            caps_lock: CapsMode::Custom,
            caps_custom: vec![0xA2, 0xA4, 0x4B],
            screen: r"\\.\DISPLAY2".into(),
            show_over_fullscreen: true,
            accent: Accent::Purple,
        };
        let json = serde_json::to_string(&saved).unwrap_or_default();
        let loaded: Option<Settings> = serde_json::from_str(&json).ok();
        assert_eq!(
            loaded.map(|s| serde_json::to_string(&s).unwrap_or_default()),
            Some(json)
        );
    }
}
