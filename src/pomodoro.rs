//! A Pomodoro timer: a focus session, then a break that starts by itself.
//!
//! Time is passed in rather than read, so the logic is testable without waiting.

use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Focus,
    Break,
}

#[derive(Debug)]
pub struct Pomodoro {
    pub phase: Phase,
    focus: Duration,
    rest: Duration,
    /// When the running phase ends; `None` while paused or stopped.
    ends_at: Option<Instant>,
    /// Time left in the phase while paused.
    left: Duration,
    /// Focus sessions finished since BUKU started.
    pub sessions: u32,
}

impl Pomodoro {
    pub fn new(focus_minutes: u32, break_minutes: u32) -> Self {
        let focus = Duration::from_secs(u64::from(focus_minutes) * 60);
        Self {
            phase: Phase::Focus,
            focus,
            rest: Duration::from_secs(u64::from(break_minutes) * 60),
            ends_at: None,
            left: focus,
            sessions: 0,
        }
    }

    /// New lengths from Settings. A running phase keeps its end time; a paused or
    /// stopped one starts over at the new length.
    pub fn set_lengths(&mut self, focus_minutes: u32, break_minutes: u32) {
        self.focus = Duration::from_secs(u64::from(focus_minutes) * 60);
        self.rest = Duration::from_secs(u64::from(break_minutes) * 60);
        if self.ends_at.is_none() {
            self.left = self.length();
        }
    }

    pub fn is_running(&self) -> bool {
        self.ends_at.is_some()
    }

    fn length(&self) -> Duration {
        match self.phase {
            Phase::Focus => self.focus,
            Phase::Break => self.rest,
        }
    }

    pub fn remaining(&self, now: Instant) -> Duration {
        match self.ends_at {
            Some(end) => end.saturating_duration_since(now),
            None => self.left,
        }
    }

    /// How far through the current phase, 0..1.
    pub fn progress(&self, now: Instant) -> f32 {
        let length = self.length().as_secs_f32();
        if length <= 0.0 {
            return 0.0;
        }
        // A session shortened while running can have more left than its new length.
        (1.0 - self.remaining(now).as_secs_f32() / length).clamp(0.0, 1.0)
    }

    /// Starts or pauses. If the phase already ran out (the once-a-second check
    /// hadn't caught it yet), it finishes instead, and the ended phase is returned.
    pub fn toggle(&mut self, now: Instant) -> Option<Phase> {
        if self.ends_at.is_some_and(|end| now >= end) {
            return self.poll(now);
        }
        match self.ends_at.take() {
            Some(end) => self.left = end.saturating_duration_since(now),
            None => self.ends_at = Some(now + self.left),
        }
        None
    }

    /// Stops and goes back to a full focus session.
    pub fn reset(&mut self) {
        self.phase = Phase::Focus;
        self.ends_at = None;
        self.left = self.focus;
    }

    /// Moves on when the running phase is over and returns the phase that ended.
    /// A finished focus session starts the break; a finished break waits for the user.
    pub fn poll(&mut self, now: Instant) -> Option<Phase> {
        let end = self.ends_at?;
        if now < end {
            return None;
        }
        let finished = self.phase;
        match finished {
            Phase::Focus => {
                self.sessions += 1;
                self.phase = Phase::Break;
                self.left = self.rest;
                self.ends_at = Some(now + self.rest);
            }
            Phase::Break => self.reset(),
        }
        Some(finished)
    }
}

/// `m:ss`, rounded up so a running timer never shows 0:00 before it ends.
pub fn format_remaining(left: Duration) -> String {
    let secs = u64::try_from(left.as_nanos().div_ceil(1_000_000_000)).unwrap_or(u64::MAX);
    format!("{}:{:02}", secs / 60, secs % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toggle_should_pause_with_the_time_left() {
        let start = Instant::now();
        let mut timer = Pomodoro::new(25, 5);
        timer.toggle(start);
        timer.toggle(start + Duration::from_secs(60));
        assert_eq!(
            timer.remaining(start + Duration::from_secs(600)),
            Duration::from_secs(24 * 60)
        );
    }

    #[test]
    fn poll_should_start_the_break_after_focus() {
        let start = Instant::now();
        let mut timer = Pomodoro::new(25, 5);
        timer.toggle(start);
        let ended = timer.poll(start + Duration::from_secs(25 * 60));
        assert_eq!(
            (ended, timer.phase, timer.is_running(), timer.sessions),
            (Some(Phase::Focus), Phase::Break, true, 1)
        );
    }

    #[test]
    fn poll_should_stop_on_a_full_focus_after_the_break() {
        let start = Instant::now();
        let mut timer = Pomodoro::new(25, 5);
        timer.toggle(start);
        timer.poll(start + Duration::from_secs(25 * 60));
        let ended = timer.poll(start + Duration::from_secs(30 * 60));
        assert_eq!(
            (
                ended,
                timer.phase,
                timer.is_running(),
                timer.remaining(start)
            ),
            (
                Some(Phase::Break),
                Phase::Focus,
                false,
                Duration::from_secs(25 * 60)
            )
        );
    }

    #[test]
    fn set_lengths_should_keep_a_running_session() {
        let start = Instant::now();
        let mut timer = Pomodoro::new(25, 5);
        timer.toggle(start);
        timer.set_lengths(50, 10);
        assert_eq!(timer.remaining(start), Duration::from_secs(25 * 60));
    }

    #[test]
    fn set_lengths_should_reset_a_stopped_timer() {
        let mut timer = Pomodoro::new(25, 5);
        timer.set_lengths(50, 10);
        assert_eq!(
            timer.remaining(Instant::now()),
            Duration::from_secs(50 * 60)
        );
    }

    #[test]
    fn format_remaining_should_round_up_partial_seconds() {
        assert_eq!(format_remaining(Duration::from_millis(59_001)), "1:00");
    }
}

#[cfg(test)]
mod edge_case_tests {
    use super::*;

    #[test]
    fn progress_should_stay_between_zero_and_one_after_shortening_a_running_session() {
        let start = Instant::now();
        let mut timer = Pomodoro::new(25, 5);
        timer.toggle(start);
        timer.set_lengths(5, 1);
        let progress = timer.progress(start);
        assert!((0.0..=1.0).contains(&progress), "progress {progress}");
    }

    #[test]
    fn pausing_just_after_the_end_should_finish_the_session_instead_of_freezing_at_zero() {
        let start = Instant::now();
        let mut timer = Pomodoro::new(25, 5);
        timer.toggle(start);
        timer.toggle(start + Duration::from_secs(25 * 60 + 10));
        assert_eq!((timer.phase, timer.sessions), (Phase::Break, 1));
    }

    #[test]
    fn under_a_millisecond_left_should_still_show_one_second() {
        assert_eq!(format_remaining(Duration::from_micros(500)), "0:01");
    }
}
