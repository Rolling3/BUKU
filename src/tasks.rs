//! The to-do list, stored in `%APPDATA%\BUKU\tasks.json`.

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use crate::store;

const FILE: &str = "tasks.json";
/// Enough for a day's list; the card shows a few at a time.
pub const MAX_TASKS: usize = 50;
pub const MAX_TASK_CHARS: usize = 200;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Task {
    pub text: String,
    pub done: bool,
}

#[derive(Debug, Default, Serialize)]
pub struct TaskList {
    tasks: Vec<Task>,
}

/// Read task by task: an entry that can't be used is skipped rather than losing
/// the whole list, which the next change would then overwrite.
impl<'de> Deserialize<'de> for TaskList {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        let tasks = value
            .get("tasks")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| {
                        // The file may have been edited by hand: same rules as typed text.
                        let text = clean_text(item.get("text")?.as_str()?);
                        let done = item.get("done").and_then(Value::as_bool).unwrap_or(false);
                        (!text.is_empty()).then_some(Task { text, done })
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(Self { tasks })
    }
}

impl TaskList {
    pub fn load() -> Self {
        let mut list: Self = store::load(FILE);
        list.tasks.truncate(MAX_TASKS);
        list
    }

    fn save(&self) {
        // A failed save keeps the list in memory; nothing is worth an error box here.
        let _ = store::save(FILE, self);
    }

    pub fn get(&self, index: usize) -> Option<&Task> {
        self.tasks.get(index)
    }

    /// Indices in display order: open tasks first, then finished ones, each in the
    /// order they were added.
    pub fn display_order(&self) -> Vec<usize> {
        let open = (0..self.tasks.len()).filter(|&i| !self.tasks[i].done);
        let done = (0..self.tasks.len()).filter(|&i| self.tasks[i].done);
        open.chain(done).collect()
    }

    pub fn is_full(&self) -> bool {
        self.tasks.len() >= MAX_TASKS
    }

    pub fn any_done(&self) -> bool {
        self.tasks.iter().any(|task| task.done)
    }

    /// Adds a task; blank text is ignored. Returns false when the list is full.
    pub fn add(&mut self, text: &str) -> bool {
        let text = clean_text(text);
        if text.is_empty() {
            return true;
        }
        if self.tasks.len() >= MAX_TASKS {
            return false;
        }
        self.tasks.push(Task { text, done: false });
        self.save();
        true
    }

    pub fn toggle(&mut self, index: usize) {
        if let Some(task) = self.tasks.get_mut(index) {
            task.done = !task.done;
            self.save();
        }
    }

    pub fn remove(&mut self, index: usize) {
        if index < self.tasks.len() {
            self.tasks.remove(index);
            self.save();
        }
    }

    pub fn clear_done(&mut self) {
        self.tasks.retain(|task| !task.done);
        self.save();
    }
}

/// Control characters, the bidi marks, embedding, override and isolate controls,
/// which can reorder text, and characters that show nothing at all (zero-width
/// space, word joiner, byte order mark, blank braille, Hangul fillers, tags),
/// which can make text look empty. The joiners U+200C and U+200D stay: Arabic
/// and Persian need them.
fn is_unwanted(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{061C}'
                | '\u{115F}'
                | '\u{1160}'
                | '\u{180E}'
                | '\u{200B}'
                | '\u{200E}'
                | '\u{200F}'
                | '\u{202A}'..='\u{202E}'
                | '\u{2060}'..='\u{2064}'
                | '\u{2066}'..='\u{2069}'
                | '\u{2800}'
                | '\u{3164}'
                | '\u{FEFF}'
                | '\u{FFA0}'
                | '\u{E0000}'..='\u{E007F}'
        )
}

/// One line, single spaces, at most `MAX_TASK_CHARS` characters.
fn clean_text(text: &str) -> String {
    clean_line(text, MAX_TASK_CHARS)
}

/// One line, single spaces, at most `max_chars` characters, without control or
/// direction characters. Spotify shortcut names follow the same rules.
pub fn clean_line(text: &str, max_chars: usize) -> String {
    // Whitespace controls (tab, newline) must still separate words, so they are kept for the split.
    let text: String = text
        .chars()
        .filter(|&c| c.is_whitespace() || !is_unwanted(c))
        .collect();
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let capped: String = text.chars().take(max_chars).collect();
    // The cut can land just after a space.
    capped.trim_end().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(items: &[(&str, bool)]) -> TaskList {
        TaskList {
            tasks: items
                .iter()
                .map(|(text, done)| Task {
                    text: (*text).into(),
                    done: *done,
                })
                .collect(),
        }
    }

    #[test]
    fn display_order_should_put_open_tasks_first() {
        let tasks = list(&[("a", true), ("b", false), ("c", true), ("d", false)]);
        assert_eq!(tasks.display_order(), vec![1, 3, 0, 2]);
    }

    #[test]
    fn clean_text_should_collapse_whitespace_and_cap_length() {
        let cleaned = clean_text(&format!(
            "  buy
 milk {}",
            "x".repeat(500)
        ));
        assert!(cleaned.starts_with("buy milk x") && cleaned.chars().count() == MAX_TASK_CHARS);
    }
}

#[cfg(test)]
mod sanitize_tests {
    use super::*;

    #[test]
    fn clean_text_should_drop_control_and_bidi_characters() {
        let cleaned = clean_text("a\u{0}b\u{7}c \u{202E}d\u{2066}e\u{2069}f\u{1b}g");
        assert_eq!(cleaned, "abc defg");
    }

    #[test]
    fn text_that_shows_nothing_should_clean_to_nothing() {
        assert_eq!(
            clean_text("\u{2800}\u{200B}\u{200F}\u{FEFF}\u{3164}\u{E0041}\u{2060}"),
            ""
        );
        assert_eq!(clean_text("a\u{200E}b\u{061C}c"), "abc");
    }

    #[test]
    fn clean_text_should_keep_arabic_joiners() {
        let text = "\u{0645}\u{200C}\u{0646}\u{200D}";
        assert_eq!(clean_text(text), text);
    }
}

#[cfg(test)]
mod load_tests {
    use super::*;

    #[test]
    fn one_malformed_task_should_not_lose_the_others() {
        let json = r#"{"tasks":[{"text":"a","done":true},{"text":"b"},{"text":"c","done":false},{"done":true}]}"#;
        let list: Option<TaskList> = serde_json::from_str(json).ok();
        let texts: Option<Vec<String>> =
            list.map(|l| l.tasks.into_iter().map(|t| t.text).collect());
        assert_eq!(texts, Some(vec!["a".into(), "b".into(), "c".into()]));
    }
}

#[cfg(test)]
mod edge_case_tests {
    use super::*;

    #[test]
    fn loaded_tasks_should_be_cleaned_like_typed_ones() {
        let long = "x".repeat(5_000);
        let json = format!(r#"{{"tasks":[{{"text":"{long}\ntail","done":false}}]}}"#);
        let list: Option<TaskList> = serde_json::from_str(&json).ok();
        let text = list
            .and_then(|l| l.tasks.first().map(|t| t.text.clone()))
            .unwrap_or_default();
        assert!(
            text.chars().count() <= MAX_TASK_CHARS && !text.contains('\n'),
            "{} chars",
            text.chars().count()
        );
    }

    #[test]
    fn cutting_a_task_at_a_space_should_not_leave_a_trailing_space() {
        let cleaned = clean_text(&format!("{} tail", "a".repeat(MAX_TASK_CHARS - 1)));
        assert_eq!(cleaned, clean_text(&cleaned));
    }
}
