//! Glasses of water drunk today, stored in `%APPDATA%\BUKU\water.json`.

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use windows::Win32::System::SystemInformation::GetLocalTime;

use crate::store;

const FILE: &str = "water.json";
const MAX_GLASSES: u32 = 99;
/// How long without a logged glass before a reminder.
pub const REMINDER_INTERVAL: Duration = Duration::from_secs(60 * 60);
/// Reminders only between these local hours, so nobody is nudged at 2 am.
const DAY_HOURS: std::ops::Range<u16> = 9..22;

#[derive(Debug, Default, Serialize)]
pub struct WaterLog {
    /// Local date the count belongs to, `YYYY-MM-DD`.
    date: String,
    glasses: u32,
    /// Seconds since 1970 of the last logged glass.
    last_drink: Option<u64>,
}

/// Read field by field, like the other files: one unusable value doesn't throw
/// away today's count.
impl<'de> Deserialize<'de> for WaterLog {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        Ok(Self {
            date: value
                .get("date")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            glasses: value
                .get("glasses")
                .and_then(Value::as_u64)
                .map_or(0, |n| n.min(u64::from(MAX_GLASSES)) as u32),
            last_drink: value.get("last_drink").and_then(Value::as_u64),
        })
    }
}

impl WaterLog {
    pub fn load() -> Self {
        store::load(FILE)
    }

    fn save(&self) {
        // Losing a count to a full disk isn't worth interrupting anyone over.
        let _ = store::save(FILE, self);
    }

    /// Today's count; a log from an earlier day counts as zero.
    pub fn glasses(&self, today: &str) -> u32 {
        // Capped here too, not only when counting: the file may hold anything.
        if self.date == today {
            self.glasses.min(MAX_GLASSES)
        } else {
            0
        }
    }

    pub fn add(&mut self, today: &str, now_unix: u64) {
        self.glasses = self.glasses(today).saturating_add(1).min(MAX_GLASSES);
        self.date = today.to_owned();
        self.last_drink = Some(now_unix);
        self.save();
    }

    pub fn remove(&mut self, today: &str) {
        self.glasses = self.glasses(today).saturating_sub(1);
        self.date = today.to_owned();
        self.save();
    }

    /// Time until the next reminder is due, measured from the last glass, or from
    /// `started_unix` when nothing was logged since.
    pub fn until_reminder(&self, now_unix: u64, started_unix: u64) -> Duration {
        // A last glass "in the future" comes from a clock set back or a damaged file;
        // counting it as now keeps reminders at most an hour apart either way.
        let last = self.last_drink.unwrap_or(started_unix).min(now_unix);
        let since = last.max(started_unix.min(now_unix));
        let due = since.saturating_add(REMINDER_INTERVAL.as_secs());
        Duration::from_secs(due.saturating_sub(now_unix))
    }
}

pub struct LocalNow {
    pub date: String,
    pub hour: u16,
}

pub fn local_now() -> LocalNow {
    let time = unsafe { GetLocalTime() };
    LocalNow {
        date: format!("{:04}-{:02}-{:02}", time.wYear, time.wMonth, time.wDay),
        hour: time.wHour,
    }
}

pub fn is_daytime(hour: u16) -> bool {
    DAY_HOURS.contains(&hour)
}

pub fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glasses_should_reset_on_a_new_day() {
        let log = WaterLog {
            date: "2026-10-31".into(),
            glasses: 5,
            last_drink: None,
        };
        assert_eq!(log.glasses("2026-11-01"), 0);
    }

    #[test]
    fn until_reminder_should_count_from_the_last_glass() {
        let log = WaterLog {
            date: String::new(),
            glasses: 0,
            last_drink: Some(10_000),
        };
        assert_eq!(log.until_reminder(10_600, 0), Duration::from_secs(3000));
    }

    #[test]
    fn until_reminder_should_count_from_start_when_the_last_glass_is_older() {
        let log = WaterLog {
            date: String::new(),
            glasses: 0,
            last_drink: Some(10),
        };
        assert_eq!(log.until_reminder(5_000, 4_000), Duration::from_secs(2600));
    }
}

#[cfg(test)]
mod stored_value_tests {
    use super::*;

    const NOW: u64 = 1_793_000_000;
    const HOUR: Duration = REMINDER_INTERVAL;

    fn log(glasses: u32, last_drink: Option<u64>) -> WaterLog {
        WaterLog {
            date: "2026-11-01".into(),
            glasses,
            last_drink,
        }
    }

    #[test]
    fn a_huge_stored_last_glass_should_not_repeat_the_reminder_every_second() {
        // On a reminder the app restarts the count from now.
        let wait = log(0, Some(u64::MAX - 10)).until_reminder(NOW, NOW);
        assert_eq!(wait, HOUR);
    }

    #[test]
    fn a_last_glass_in_the_future_should_not_silence_reminders_past_an_hour() {
        let wait = log(0, Some(NOW + 86_400)).until_reminder(NOW, NOW - 5_000);
        assert!(wait <= HOUR, "silenced for {wait:?}");
    }

    #[test]
    fn a_stored_count_should_be_capped_like_a_counted_one() {
        assert_eq!(log(5_000, None).glasses("2026-11-01"), 99);
    }
}

#[cfg(test)]
mod edge_case_tests {
    use super::*;

    #[test]
    fn one_unreadable_field_should_not_lose_todays_count() {
        let json = r#"{"date":"2026-11-01","glasses":3,"last_drink":"yesterday"}"#;
        let log: Option<WaterLog> = serde_json::from_str(json).ok();
        assert_eq!(log.map(|log| log.glasses("2026-11-01")), Some(3));
    }
}
