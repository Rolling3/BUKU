//! HUD sources, media updates and the pointer.

use super::*;

/// Whether a new pop-up replaces the one in `self.hud`. Over an open card a
/// plain pop-up (volume, brightness, Caps Lock, language) can't be seen, so it
/// must not take the place of a notice that is showing over the card.
fn replaces_hud(notice_showing: bool, new_is_notice: bool, card_open: bool) -> bool {
    !(notice_showing && !new_is_notice && card_open)
}

/// Whether a pop-up or notice is dropped: with "Automatically hide BUKU" on
/// and the card closed, the notch shows nothing, and nothing waits to show.
fn dropped_while_hidden(auto_hide: bool, card_open: bool) -> bool {
    auto_hide && !card_open
}

/// What to save after the notch was dragged to another screen: the file's
/// settings (`saved`) with the new screen, nothing when the file already has
/// it, or the settings in use when the file can't be read (`None`), never
/// defaults over the user's settings.
fn screen_save(saved: Option<Settings>, in_use: &Settings) -> Option<Settings> {
    match saved {
        Some(saved) if saved.screen == in_use.screen => None,
        Some(mut saved) => {
            saved.screen = in_use.screen.clone();
            Some(saved)
        }
        None => Some(in_use.clone()),
    }
}

/// What a right-click on `button` does: on a Spotify shortcut (its x
/// included), removes it; `None` elsewhere, where the menu opens.
pub(super) fn right_click(button: Option<Button>) -> Option<Button> {
    match button {
        Some(Button::SpotifyOpen(index) | Button::SpotifyRemove(index)) => {
            Some(Button::SpotifyRemove(index))
        }
        _ => None,
    }
}

/// Buttons that act on a list by position. After one removes or ticks an item
/// the next slides into its place, so the second click of a double-click is
/// ignored for them.
fn is_list_click(button: Option<Button>) -> bool {
    matches!(
        button,
        Some(
            Button::TaskToggle(_)
                | Button::TaskDelete(_)
                | Button::TaskClearDone
                | Button::ShelfOpen(_)
                | Button::ShelfRemove(_)
                | Button::ShelfClear
                | Button::SpotifyOpen(_)
                | Button::SpotifyRemove(_)
        )
    )
}

/// Whether moving to `page` must close the open text box (`input_page`, the
/// page it belongs to): staying open under another page would add a task or a
/// shortcut unseen.
fn closes_input(input_page: Option<Page>, page: Page) -> bool {
    input_page.is_some_and(|own| own != page)
}

/// What a link typed into the Spotify box leads to.
#[derive(Debug, PartialEq)]
enum NewShortcut {
    /// Not a Spotify link, already there, or no room: nothing is added.
    Refused,
    /// Added unnamed; Spotify is asked for the name.
    AddAndLookUp(spotify::Link),
    /// A second box asks for the name.
    AskName(spotify::Link),
}

fn new_shortcut(text: &str, list: &spotify::Shortcuts, look_up_names: bool) -> NewShortcut {
    match spotify::Link::parse(text) {
        Some(link) if !list.contains(&link) && !list.is_full() => {
            if look_up_names {
                NewShortcut::AddAndLookUp(link)
            } else {
                NewShortcut::AskName(link)
            }
        }
        _ => NewShortcut::Refused,
    }
}

/// Whether a Shelf file left after being dragged out. A drag that never ran (the
/// file couldn't be reached) keeps it; a move takes it away; a drop that left a
/// local file gone also does (Explorer reports some moves that way). A network
/// path isn't looked up, as that can hang on an unreachable share.
fn gone_after_drag(effect: Option<DROPEFFECT>, local: bool, exists: impl FnOnce() -> bool) -> bool {
    match effect {
        None => false,
        Some(effect) if effect.0 & DROPEFFECT_MOVE.0 != 0 => true,
        Some(_) => local && !exists(),
    }
}

/// What the Caps Lock indicator should show after a press: the flipped state, or
/// nothing while the key is remapped, since the hook swallows it and Caps Lock
/// never toggles.
fn caps_press_shows(taken_over: bool, caps_on: bool) -> Option<bool> {
    (!taken_over).then_some(!caps_on)
}

/// Delay for the lyric timer. A hostile lyrics file can put the next line days away,
/// which must not become a short delay: Windows runs any interval above
/// USER_TIMER_MAXIMUM every 10 ms.
fn lyric_timer_ms(ms: u64) -> u32 {
    const MAXIMUM: u64 = 0x7FFF_FFFF;
    ms.clamp(15, MAXIMUM) as u32
}

/// The line to show at `position_ms`, and how long until the next one.
/// Each line shows `early_ms` before its time: synced lyrics mark when singing
/// starts, so a line shown only then appears after the words have begun. A line
/// never replaces the previous one before halfway through it, so a line shorter
/// than the lead (a song's first, often) still shows.
fn lyric_schedule(
    lines: &[lyrics::Line],
    position_ms: u64,
    early_ms: u32,
) -> (Option<usize>, Option<u64>) {
    let shows_from = |index: usize| {
        let time = lines[index].time_ms;
        let early = time.saturating_sub(u64::from(early_ms));
        match index.checked_sub(1).map(|previous| lines[previous].time_ms) {
            Some(previous) => early.max(previous + time.saturating_sub(previous) / 2),
            None => early,
        }
    };
    // `shows_from` never decreases along the (sorted) lines.
    let shown = (0..lines.len())
        .take_while(|&index| shows_from(index) <= position_ms)
        .count();
    let next_in = (shown < lines.len()).then(|| shows_from(shown) - position_ms);
    (shown.checked_sub(1), next_in)
}

/// `lyric_schedule` at a position that is negative before shifted lyrics begin:
/// then nothing shows yet, and the first line comes once playback gets there.
fn lyric_schedule_at(
    lines: &[lyrics::Line],
    position_ms: i64,
    early_ms: u32,
) -> (Option<usize>, Option<u64>) {
    match u64::try_from(position_ms) {
        Ok(position) => lyric_schedule(lines, position, early_ms),
        Err(_) => {
            let first = lines
                .first()
                .map(|line| line.time_ms.saturating_sub(u64::from(early_ms)));
            (
                None,
                first.map(|first| first.saturating_add(position_ms.unsigned_abs())),
            )
        }
    }
}

impl App {
    // ----- HUDs -----

    pub(super) fn show_hud(&mut self, hud: Hud) {
        let is_notice = |hud: &Hud| matches!(hud, Hud::Pomodoro { .. } | Hud::Water { .. });
        let card_open = self.hover || self.pinned || self.input.is_some() || self.drag_over;
        // Hidden, nothing pops up; kept for later, a notice would show in place
        // of the card the moment the pointer brings it out.
        if dropped_while_hidden(self.settings.auto_hide, card_open) {
            return;
        }
        let notice_showing = self.hud.as_ref().is_some_and(is_notice);
        if !replaces_hud(notice_showing, is_notice(&hud), card_open) {
            return;
        }
        let duration = match hud {
            Hud::Volume { .. } | Hud::Brightness(_) | Hud::CapsLock(_) | Hud::Language(_) => HUD_MS,
            _ => NOTICE_MS,
        };
        if self.shown == View::Hud {
            self.shown_hud = Some(hud.clone());
        }
        self.hud = Some(hud);
        unsafe {
            SetTimer(Some(self.hwnd), TIMER_HUD, duration, None);
        }
        if self.shown == View::Hud {
            self.render();
        }
        self.retarget();
    }

    pub(super) fn on_volume(&mut self, level: usize, flags: isize) {
        // Still queued from the playback device that was the default before.
        if self
            .audio
            .as_ref()
            .is_some_and(|audio| audio.generation() != volume_generation(flags))
        {
            return;
        }
        let muted = flags & VOLUME_MUTED != 0;
        // Core Audio also reports per-channel and balance changes; only show real changes.
        if self.last_volume == Some((level, muted)) {
            return;
        }
        self.last_volume = Some((level, muted));
        self.show_hud(Hud::Volume {
            level: level as f32 / 1000.0,
            muted,
        });
    }

    // ----- moving between screens -----

    /// Starts dragging the notch: it jumps to whichever screen the pointer is
    /// over, and stays there when the button is released.
    /// The mouse is already held since the press (see WM_LBUTTONDOWN); taking it
    /// again would tell this window it lost it.
    fn begin_screen_drag(&mut self) {
        self.drag_armed = false;
        self.screen_drag = true;
    }

    fn follow_pointer_to_screen(&mut self) {
        let mut point = POINT::default();
        if unsafe { GetCursorPos(&mut point) }.is_err() {
            return;
        }
        let Some(screen) = display::at(point) else {
            return;
        };
        let setting = screen.setting();
        if setting != self.settings.screen {
            self.settings.screen = setting;
            self.move_to_screen();
        }
    }

    /// Ends a drag between screens and remembers where the notch ended up.
    /// `release`: let go of the mouse. Not when another window just took it (a
    /// menu, say): releasing is per thread and would take it from that window.
    pub(super) fn end_screen_drag(&mut self, release: bool) {
        if !std::mem::take(&mut self.screen_drag) {
            return;
        }
        // Released through wndproc, once this borrow of the App has ended.
        self.release_capture = release;
        // Only the screen changes: keep anything else as the file has it, in case
        // the Settings window saved a change meanwhile.
        // A file that can't be read is written over with the settings in use, not
        // with defaults; the unreadable one was kept aside as `.bad`.
        if let Some(saved) = screen_save(Settings::read_for_update(), &self.settings)
            && saved.save().is_ok()
        {
            // An open Settings window shows the new screen, and won't put the
            // old one back with its next save.
            unsafe {
                if let Ok(window) = FindWindowW(w!("BUKU.Settings"), None) {
                    WindowHandle::new(window).post(msg::SETTINGS_RELOAD, 0, 0);
                }
            }
        }
        if !self.pointer_over_window() {
            self.on_mouse_leave();
        }
    }

    // ----- volume bar -----

    /// The playing app's volume changed, or its audio session ended.
    pub(super) fn on_app_volume(&mut self, flags: isize) {
        // Still queued from the sessions of an earlier lookup.
        if volume_generation(flags) != self.app_volume_generation & 0xff {
            return;
        }
        if flags & APP_VOLUME_GONE != 0 {
            self.app_volume = None;
            self.refresh_app_volume(true);
            return;
        }
        if flags & VOLUME_OWN != 0 {
            // Mid-drag, each step's notification arrives after later steps were
            // drawn; the bar keeps the pointer's level and catches up at the end.
            // Otherwise it may be an earlier step than the last one, so show where
            // the app's volume is now, not this one.
            if !self.volume_drag {
                self.sync_volume();
            }
            return;
        }
        // Changed in the player or the Volume mixer. Read it back rather than
        // trust the message: a browser's sessions may each say something else.
        self.sync_volume();
    }

    /// Finds the playing app's audio sessions again when the player changed, or
    /// always with `force` (the card is opening; the app may have started or
    /// stopped making sound since).
    pub(super) fn refresh_app_volume(&mut self, force: bool) {
        let key = {
            let state = media::lock(&self.media.state);
            (state.source_app.clone(), state.playing)
        };
        // Search again when the player or its playing state changed (its sound
        // starts a moment after it says it plays), and keep trying while it plays
        // with no session found. Media messages come only on real changes.
        let missing = self.app_volume.is_none() && key.1 && !key.0.is_empty();
        if force || missing || self.app_volume_for != key {
            // Unregister the old sessions before registering the new ones.
            self.app_volume = None;
            self.app_volume_generation = self.app_volume_generation.wrapping_add(1);
            let generation = self.app_volume_generation;
            self.app_volume = self.audio.as_ref().and_then(|audio| {
                AppVolume::find(
                    audio.enumerator(),
                    &key.0,
                    WindowHandle::new(self.hwnd),
                    generation,
                )
            });
            self.app_volume_for = key;
        }
        self.sync_volume();
        if self.app_volume.is_none()
            && self.app_level.take().is_some()
            && self.shown == View::Card
            && !self.animating
        {
            self.render();
        }
    }

    /// Sets the system volume from the pointer's position over the card's volume bar.
    pub(super) fn set_volume_at(&mut self, lparam: LPARAM) {
        let (x, _) = self.pointer_dips(lparam);
        let level = draw::volume_at_canvas(self.width.value, x);
        self.set_volume(level, true);
    }

    /// Sets the system volume and shows it on the bar right away, instead of
    /// waiting for Core Audio's notification.
    fn set_volume(&mut self, level: f32, unmute: bool) {
        let Some(app) = &self.app_volume else { return };
        let Some((_, muted)) = app.level() else {
            return;
        };
        let level = level.clamp(0.0, 1.0);
        app.set_level(level, unmute);
        let shown = (
            (level * 1000.0).round() as usize,
            muted && !(unmute && level > 0.0),
        );
        if self.app_level != Some(shown) {
            self.app_level = Some(shown);
            if self.shown == View::Card && !self.animating {
                self.render();
            }
        }
    }

    /// Starts dragging the volume bar. The capture keeps the drag going when the
    /// pointer slides off the notch.
    pub(super) fn begin_volume_drag(&mut self, lparam: LPARAM) {
        self.volume_drag = true;
        unsafe {
            SetCapture(self.hwnd);
        }
        self.set_volume_at(lparam);
    }

    pub(super) fn end_volume_drag(&mut self) {
        if !std::mem::take(&mut self.volume_drag) {
            return;
        }
        // Releasing the capture sends WM_CAPTURECHANGED back into the window
        // procedure, so wndproc does it once this borrow of the App has ended.
        self.release_capture = true;
        self.after_volume_drag();
    }

    /// Catches the bar up on the notifications skipped during a drag, and starts
    /// the close if the drag ended off the notch: no WM_MOUSEMOVE will come for it.
    pub(super) fn after_volume_drag(&mut self) {
        self.sync_volume();
        if !self.pointer_over_window() {
            self.on_mouse_leave();
        }
    }

    /// Shows the playing app's current volume on the bar.
    fn sync_volume(&mut self) {
        if let Some(level) = self.app_volume.as_ref().and_then(AppVolume::level)
            && self.app_level != Some(level)
        {
            self.app_level = Some(level);
            if self.shown == View::Card && !self.animating {
                self.render();
            }
        }
    }

    /// The mouse wheel over the volume row nudges the volume, 2% per notch.
    /// Returns false when the pointer isn't over it, so the wheel goes on as usual.
    pub(super) fn on_mouse_wheel(&mut self, wparam: WPARAM, lparam: LPARAM) -> bool {
        let mut point = POINT {
            x: (lparam.0 & 0xffff) as i16 as i32,
            y: ((lparam.0 >> 16) & 0xffff) as i16 as i32,
        };
        unsafe {
            let _ = ScreenToClient(self.hwnd, &mut point);
        }
        let client = LPARAM(((point.y as u16 as isize) << 16) | point.x as u16 as isize);
        if !matches!(self.button_at(client), Some(Button::Volume | Button::Mute)) {
            return false;
        }
        // Start from the device's level, not the bar's: notifications for earlier
        // notches may not have arrived yet.
        let Some((level, _)) = self.app_volume.as_ref().and_then(AppVolume::level) else {
            return false;
        };
        let notches = ((wparam.0 >> 16) & 0xffff) as i16 as f32 / WHEEL_DELTA as f32;
        // Turning it down never unmutes.
        self.set_volume(level as f32 / 1000.0 + notches * 0.02, notches > 0.0);
        true
    }

    pub(super) fn on_power_setting(&mut self, lparam: LPARAM) {
        let setting = lparam.0 as *const POWERBROADCAST_SETTING;
        if setting.is_null() {
            return;
        }
        // Read field by field through the raw pointer: the block comes from the
        // message sender, so no reference to it is formed.
        let (guid, length) = unsafe {
            (
                std::ptr::read_unaligned(std::ptr::addr_of!((*setting).PowerSetting)),
                std::ptr::read_unaligned(std::ptr::addr_of!((*setting).DataLength)),
            )
        };
        if length < 4 {
            return;
        }
        let value =
            unsafe { std::ptr::read_unaligned(std::ptr::addr_of!((*setting).Data).cast::<u32>()) };
        match guid {
            GUID_ACDC_POWER_SOURCE if self.power_hud_registered => {
                return self.on_power_source(value == 0);
            }
            GUID_BATTERY_PERCENTAGE_REMAINING if self.power_hud_registered => {
                return self.on_battery_percent(value.min(100));
            }
            GUID_VIDEO_BRIGHTNESS_SETTING => {}
            _ => return,
        }
        let value = value.min(100);
        // Windows sends the current value right after registration; that isn't a change.
        let previous = self.last_brightness.replace(value);
        if previous.is_some() && previous != Some(value) {
            self.show_hud(Hud::Brightness(value as f32 / 100.0));
        }
    }

    /// `plugged` is true on AC power. Windows reports the current source once at
    /// registration; only later changes are news.
    fn on_power_source(&mut self, plugged: bool) {
        let previous = self.on_ac.replace(plugged);
        if previous.is_some() && previous != Some(plugged) {
            self.show_hud(Hud::Power {
                charging: plugged,
                percent: system::battery_percent(),
                low: false,
            });
        }
    }

    fn on_battery_percent(&mut self, percent: u32) {
        let previous = self.battery.replace(percent);
        let on_battery = self.on_ac == Some(false);
        // Warn once when the charge drops through 20% and again through 10%.
        let crossed = previous.is_some_and(|before| {
            LOW_BATTERY_LEVELS
                .iter()
                .any(|&level| before > level && percent <= level)
        });
        if on_battery && crossed {
            self.show_hud(Hud::Power {
                charging: false,
                percent: Some(percent),
                low: true,
            });
        }
    }

    /// Network changes come in bursts while a connection settles; the check waits for
    /// the burst to end.
    pub(super) fn on_network_event(&mut self) {
        unsafe {
            SetTimer(Some(self.hwnd), TIMER_NETWORK, NETWORK_SETTLE_MS, None);
        }
    }

    pub(super) fn check_network(&mut self) {
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_NETWORK);
        }
        let now = system::network();
        let previous = self.network.replace(now.clone());
        if previous.is_some() && previous.as_ref() != Some(&now) {
            self.show_hud(Hud::Network {
                connected: now.connected,
                wifi: now.wifi,
                name: now.name,
            });
        }
    }

    pub(super) fn on_audio_device_changed(&mut self) {
        let Some(audio) = &mut self.audio else { return };
        audio.attach_default();
        // No notice for the new device's level.
        self.last_volume = audio.level();
        // The player's sessions live on the old device; find them on the new one.
        self.refresh_app_volume(true);
        let Some(audio) = &self.audio else { return };
        let device = audio.default_device();
        // Windows can report the same default several times in a row.
        if device.is_some() && device != self.output {
            if let Some((name, worn)) = device.clone() {
                self.show_hud(Hud::Output { name, worn });
            }
            self.output = device;
        }
    }

    pub(super) fn check_language(&mut self) {
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_LANGUAGE);
        }
        let language = system::keyboard_language();
        let previous = self.language.replace(language);
        if previous.is_some() && previous != Some(language) {
            let name = system::language_name(language);
            if !name.is_empty() {
                self.show_hud(Hud::Language(name));
            }
        }
    }

    pub(super) fn on_raw_input(&mut self, lparam: LPARAM) {
        let mut input = RAWINPUT::default();
        let mut size = std::mem::size_of::<RAWINPUT>() as u32;
        let read = unsafe {
            GetRawInputData(
                HRAWINPUT(lparam.0 as *mut _),
                RID_INPUT,
                Some((&mut input as *mut RAWINPUT).cast()),
                &mut size,
                std::mem::size_of::<RAWINPUTHEADER>() as u32,
            )
        };
        if read == u32::MAX || input.header.dwType != RIM_TYPEKEYBOARD.0 {
            return;
        }
        // Only Caps Lock and the keys that switch keyboard language are looked at;
        // every other key is ignored right here and nothing about it is stored.
        let keyboard = unsafe { input.data.keyboard };
        let released = keyboard.Flags & RI_KEY_BREAK as u16 != 0;
        if released && LANGUAGE_SWITCH_KEYS.contains(&keyboard.VKey) {
            // Windows applies the new layout a moment after the keys come up.
            unsafe {
                SetTimer(Some(self.hwnd), TIMER_LANGUAGE, LANGUAGE_SETTLE_MS, None);
            }
            return;
        }
        if keyboard.VKey != VK_CAPITAL.0 {
            return;
        }
        if released {
            self.caps_down = false;
        } else if !self.caps_down {
            // Holding the key auto-repeats key-down; Caps Lock only toggles on the first.
            self.caps_down = true;
            if let Some(on) = caps_press_shows(capslock::is_taken_over(), self.caps_on) {
                self.caps_on = on;
                self.show_hud(Hud::CapsLock(on));
            }
        }
    }

    // ----- media -----

    pub(super) fn current_lyric(&self) -> Option<String> {
        let state = media::lock(&self.media.state);
        self.current_lyric_in(&state)
    }

    /// The offset the user set for the playing song, 0 when none.
    pub(super) fn lyric_offset(&self, state: &media::MediaState) -> i64 {
        state.track.as_ref().map_or(0, |track| {
            self.lyric_sync.offset_ms(lyric_sync::song_key(
                &track.title,
                &track.artist,
                track.duration_ms,
            ))
        })
    }

    /// Where in the lyrics file playback is, after the song's offset.
    fn lyric_position(&self, state: &media::MediaState) -> i64 {
        lyric_sync::lyric_position(state.position_ms(), self.lyric_offset(state))
    }

    pub(super) fn current_lyric_in(&self, state: &media::MediaState) -> Option<String> {
        match &state.lyrics {
            Some(Lyrics::Synced(lines)) => {
                let early = self.settings.lyrics_early_ms;
                let index = lyric_schedule_at(lines, self.lyric_position(state), early).0?;
                let text = lines[index].text.trim();
                (!text.is_empty()).then(|| text.to_owned())
            }
            _ => None,
        }
    }

    pub(super) fn on_media_changed(&mut self) {
        self.refresh_app_volume(false);
        self.schedule_lyric();
        self.on_view_shown();
        self.retarget();
        if !self.animating {
            self.render();
        }
    }

    /// Sets a one-shot timer for the moment the next lyric line starts.
    pub(super) fn schedule_lyric(&mut self) {
        let state = media::lock(&self.media.state);
        let next_in = match (&state.lyrics, state.playing) {
            (Some(Lyrics::Synced(lines)), true) => {
                let early = self.settings.lyrics_early_ms;
                let (index, next_in) = lyric_schedule_at(lines, self.lyric_position(&state), early);
                self.lyric_index = index;
                next_in
            }
            _ => {
                self.lyric_index = None;
                None
            }
        };
        drop(state);
        unsafe {
            match next_in {
                Some(ms) => {
                    SetTimer(Some(self.hwnd), TIMER_LYRIC, lyric_timer_ms(ms), None);
                }
                None => {
                    let _ = KillTimer(Some(self.hwnd), TIMER_LYRIC);
                }
            }
        }
    }

    pub(super) fn on_button(&mut self, button: Option<Button>) {
        // Rows are clicked by position. After a task is ticked or deleted the next
        // one slides into its row, so the second click of a double-click would hit a
        // different task. Ignore list clicks for one double-click interval.
        if is_list_click(button) {
            let interval = Duration::from_millis(u64::from(unsafe { GetDoubleClickTime() }));
            if self
                .tasks_changed_at
                .is_some_and(|at| at.elapsed() < interval)
            {
                return;
            }
            self.tasks_changed_at = Some(Instant::now());
            self.hot = None;
        }
        match button {
            Some(Button::Pin) => {
                self.pinned = !self.pinned;
                if !self.pinned {
                    // Hover follows where the pointer really is: gone, the card
                    // closes as a leave would; there, a pending open is settled.
                    self.set_hover(self.pointer_over_window());
                }
                self.retarget();
            }
            Some(Button::Tab(page)) => {
                self.show_page(page);
                self.hot = None;
                self.on_view_shown();
            }
            Some(Button::Previous) => self.media.previous(),
            Some(Button::Mute) => {
                if let Some(app) = &self.app_volume {
                    app.toggle_mute();
                }
            }
            // Pressing the bar already set the level.
            Some(Button::Volume) => {}
            Some(Button::PlayPause) => self.media.toggle_play_pause(),
            Some(Button::Next) => self.media.next(),
            Some(Button::FocusStartPause) => {
                if let Some(ended) = self.pomodoro.toggle(Instant::now()) {
                    self.on_pomodoro_phase_end(ended);
                }
                self.sync_pomodoro_timer();
            }
            Some(Button::FocusReset) => {
                self.pomodoro.reset();
                self.sync_pomodoro_timer();
            }
            Some(Button::WaterAdd) => {
                self.water.add(&water::local_now().date, water::unix_now());
                self.schedule_water();
            }
            Some(Button::WaterRemove) => self.water.remove(&water::local_now().date),
            Some(Button::TaskToggle(row)) => {
                if let Some(&index) = self.tasks.display_order().get(row as usize) {
                    self.tasks.toggle(index);
                }
            }
            Some(Button::TaskDelete(row)) => {
                if let Some(&index) = self.tasks.display_order().get(row as usize) {
                    self.tasks.remove(index);
                }
            }
            Some(Button::TaskClearDone) => self.tasks.clear_done(),
            Some(Button::TaskAdd) => self.ask_input(InputFor::Task),
            Some(Button::SpotifyAdd) => self.ask_input(InputFor::SpotifyLink),
            Some(Button::SpotifyOpen(index)) => {
                // Taken now; a removal before the posted message is handled must
                // not shift the click onto another shortcut.
                if let Some(shortcut) = self.spotify.items().get(usize::from(index)) {
                    self.spotify_open = Some(shortcut.link());
                    WindowHandle::new(self.hwnd).post(msg::SPOTIFY_OPEN, 0, 0);
                }
            }
            Some(Button::SpotifyRemove(index)) => {
                self.spotify.remove(usize::from(index));
                self.hot = None;
                self.render();
            }
            Some(Button::ShelfOpen(index)) => {
                // Pin the file now; the tile index may mean another file by the time
                // the posted message is handled.
                self.shelf_open = self.shelf.items().get(index as usize).cloned();
                WindowHandle::new(self.hwnd).post(msg::SHELF_OPEN, 0, 0)
            }
            Some(Button::ShelfRemove(index)) => {
                if let Some(path) = self.shelf.items().get(index as usize).cloned() {
                    self.shelf.remove(&path);
                }
            }
            Some(Button::ShelfClear) => self.shelf.clear(),
            Some(Button::LyricThisLine) => self.line_up_lyrics(Some(Step::Shown)),
            Some(Button::LyricPrevious) => self.line_up_lyrics(Some(Step::Previous)),
            Some(Button::LyricNext) => self.line_up_lyrics(Some(Step::Next)),
            Some(Button::LyricReset) => self.line_up_lyrics(None),
            None => return,
        }
        if !self.animating {
            self.render();
        }
    }

    /// Shifts the playing song's lyrics so the line `step` asks for starts now,
    /// or back to the file's own timing with `None`, and keeps that for the song.
    fn line_up_lyrics(&mut self, step: Option<Step>) {
        let state = media::lock(&self.media.state);
        let (Some(track), Some(Lyrics::Synced(lines))) = (&state.track, &state.lyrics) else {
            return;
        };
        let song = lyric_sync::song_key(&track.title, &track.artist, track.duration_ms);
        let offset = match step {
            None => 0,
            Some(step) => {
                let early = self.settings.lyrics_early_ms;
                let shown = lyric_schedule_at(lines, self.lyric_position(&state), early).0;
                let Some(time) = lyric_sync::target_time(lines, shown, step) else {
                    return;
                };
                lyric_sync::offset_starting(time, state.position_ms())
            }
        };
        drop(state);
        self.lyric_sync.set(song, offset);
        self.schedule_lyric();
    }

    // ----- Pomodoro -----

    /// The timer ticks once a second only while a session runs.
    pub(super) fn sync_pomodoro_timer(&mut self) {
        unsafe {
            if self.pomodoro.is_running() {
                SetTimer(Some(self.hwnd), TIMER_POMODORO, 1000, None);
            } else {
                let _ = KillTimer(Some(self.hwnd), TIMER_POMODORO);
            }
        }
        // The collapsed notch widens to show the countdown, or shrinks back.
        self.retarget();
    }

    pub(super) fn on_pomodoro_tick(&mut self) {
        if let Some(ended) = self.pomodoro.poll(Instant::now()) {
            self.on_pomodoro_phase_end(ended);
        }
        let visible = match self.shown {
            View::Collapsed => true,
            View::Card => self.page == Page::Focus,
            View::Hud | View::Hidden => false,
        } && !self.fullscreen;
        if visible && !self.animating {
            self.render();
        }
    }

    fn on_pomodoro_phase_end(&mut self, ended: Phase) {
        unsafe {
            let _ = MessageBeep(MB_ICONASTERISK);
        }
        self.show_hud(Hud::Pomodoro {
            focus_done: ended == Phase::Focus,
        });
        self.sync_pomodoro_timer();
    }

    // ----- water -----

    /// One timer for the next reminder; nothing runs between reminders.
    pub(super) fn schedule_water(&mut self) {
        unsafe {
            if !self.settings.water_reminders {
                let _ = KillTimer(Some(self.hwnd), TIMER_WATER);
                return;
            }
            let wait = self
                .water
                .until_reminder(water::unix_now(), self.water_since);
            let ms = wait.as_millis().clamp(1000, u128::from(u32::MAX / 2)) as u32;
            SetTimer(Some(self.hwnd), TIMER_WATER, ms, None);
        }
    }

    pub(super) fn on_water_due(&mut self) {
        let local = water::local_now();
        let glasses = self.water.glasses(&local.date);
        let goal = self.settings.water_goal;
        // At night, at the goal, or over a full-screen game: skip this one quietly.
        if water::is_daytime(local.hour) && glasses < goal && !self.fullscreen {
            self.show_hud(Hud::Water { glasses, goal });
        }
        self.water_since = water::unix_now();
        self.schedule_water();
    }

    // ----- Spotify -----

    /// Gives new shortcuts the names Spotify sent for them.
    pub(super) fn on_spotify_names(&mut self) {
        let names = match self.spotify_names.lock() {
            Ok(mut names) => std::mem::take(&mut *names),
            Err(_) => return,
        };
        let mut named = false;
        for (link, name) in names {
            named |= self.spotify.name_if_unnamed(&link, &name);
        }
        if named && self.shown == View::Card && self.page == Page::Spotify && !self.animating {
            self.render();
        }
    }

    // ----- pointer -----

    pub(super) fn pointer_dips(&self, lparam: LPARAM) -> (f32, f32) {
        let x = (lparam.0 & 0xffff) as i16 as f32;
        let y = ((lparam.0 >> 16) & 0xffff) as i16 as f32;
        (x / self.scale, y / self.scale)
    }

    pub(super) fn button_at(&self, lparam: LPARAM) -> Option<Button> {
        if self.shown != View::Card || self.content.value < 1.0 {
            return None;
        }
        let (x, y) = self.pointer_dips(lparam);
        let button = draw::button_at(self.page_kind(), self.width.value, x, y);
        // The volume row is only drawn when the playing app's sound was found.
        if matches!(button, Some(Button::Volume | Button::Mute)) && self.app_level.is_none() {
            return None;
        }
        button
    }

    /// What the current card page shows, for hit-testing.
    fn page_kind(&self) -> PageKind {
        match self.page {
            Page::Music => {
                let state = media::lock(&self.media.state);
                if state.track.is_none() {
                    return PageKind::Idle;
                }
                let lyrics = matches!(state.lyrics, Some(Lyrics::Synced(_)));
                PageKind::Music {
                    lyrics,
                    offset: lyrics && self.lyric_offset(&state) != 0,
                }
            }
            Page::Focus => PageKind::Focus,
            Page::Water => PageKind::Water,
            Page::Spotify => PageKind::Spotify {
                count: self.spotify.items().len().min(SPOTIFY_TILES) as u8,
            },
            Page::Shelf => PageKind::Shelf {
                count: self.shelf.items().len().min(SHELF_TILES) as u8,
            },
            Page::Tasks => {
                let order = self.tasks.display_order();
                let mut done = 0u8;
                for (row, &index) in order.iter().take(TASK_ROWS).enumerate() {
                    if self.tasks.get(index).is_some_and(|task| task.done) {
                        done |= 1 << row;
                    }
                }
                PageKind::Tasks {
                    rows: order.len().min(TASK_ROWS) as u8,
                    done,
                    any_done: self.tasks.any_done(),
                    can_add: !self.tasks.is_full(),
                }
            }
        }
    }

    // ----- Shelf -----

    /// Switches the card to `page`, cancelling the task text box when it leaves Tasks.
    fn show_page(&mut self, page: Page) {
        let input_page = self.input.as_ref().map(|_| self.input_for.page());
        if closes_input(input_page, page) {
            WindowHandle::new(self.hwnd).post(msg::INPUT_CLOSE, 0, 0);
        }
        self.page = page;
    }

    pub(super) fn on_drag_over(&mut self, over: bool) {
        self.drag_over = over;
        if over {
            self.show_page(Page::Shelf);
        }
        self.retarget();
        if !self.animating {
            self.render();
        }
    }

    pub(super) fn on_shelf_drop(&mut self) {
        let files: Vec<PathBuf> = self.dropped.borrow_mut().drain(..).collect();
        self.shelf.add(files);
        self.drag_over = false;
        self.show_page(Page::Shelf);
        // During the drag Windows sent no mouse messages, so check where the pointer
        // is now: the card stays open only if it's still over the notch.
        self.hover = self.pointer_over_window();
        self.tracking_mouse = false;
        self.retarget();
        if !self.animating {
            self.render();
        }
    }

    pub(super) fn after_drag_out(&mut self, path: &Path, effect: Option<DROPEFFECT>) {
        let local = crate::shelf::is_local(path);
        if gone_after_drag(effect, local, || path.exists()) {
            self.shelf.remove(path);
        }
        self.hover = self.pointer_over_window();
        self.tracking_mouse = false;
        self.retarget();
        if !self.animating {
            self.render();
        }
    }

    pub(super) fn pointer_over_window(&self) -> bool {
        let mut point = POINT::default();
        unsafe { GetCursorPos(&mut point).is_ok() && WindowFromPoint(point) == self.hwnd }
    }

    // ----- task text box -----

    /// Whether the task text box may open: the Tasks card is up and the list has room.
    pub(super) fn can_add_task(&self) -> bool {
        task_input_allowed(self.shown, self.page, self.tasks.is_full())
    }

    /// Screen position and size of the task text box: just under the open card.
    pub(super) fn input_rect(&self) -> (i32, i32, i32, i32, f32) {
        let mut window = RECT::default();
        unsafe {
            let _ = GetWindowRect(self.hwnd, &mut window);
        }
        let s = self.scale;
        let left = draw::body_left(draw::CARD_W) + 20.0;
        let x = window.left + (left * s).round() as i32;
        let y = window.top + ((draw::CARD_H + 8.0) * s).round() as i32;
        let width = ((draw::CARD_W - 40.0) * s).round() as i32;
        let height = (42.0 * s).round() as i32;
        (x, y, width, height, s)
    }

    /// Opens the text box under the card for `purpose`, from the message loop.
    fn ask_input(&mut self, purpose: InputFor) {
        if self.input.is_none() {
            self.input_next = Some(purpose);
            WindowHandle::new(self.hwnd).post(msg::INPUT_OPEN, 0, 0);
        }
    }

    /// Whether the box for `purpose` may open now.
    pub(super) fn can_open_input(&self, purpose: &InputFor) -> bool {
        match purpose {
            InputFor::Task => self.can_add_task(),
            InputFor::SpotifyLink => {
                self.shown == View::Card && self.page == Page::Spotify && !self.spotify.is_full()
            }
            // Asked for right as the link box closed, while the card may be on
            // its way to closing with it; the name box keeps it open.
            InputFor::SpotifyName(_) => self.page == Page::Spotify,
        }
    }

    pub(super) fn on_input_closed(&mut self, text: Option<String>) {
        let purpose = std::mem::replace(&mut self.input_for, InputFor::Task);
        match (purpose, text) {
            (_, None) => {}
            (InputFor::Task, Some(text)) => {
                self.tasks.add(&text);
            }
            (InputFor::SpotifyLink, Some(text)) => {
                match new_shortcut(&text, &self.spotify, self.settings.spotify_names) {
                    NewShortcut::Refused => unsafe {
                        let _ = MessageBeep(MB_ICONASTERISK);
                    },
                    NewShortcut::AddAndLookUp(link) => {
                        if self.spotify.add(&link, "") {
                            spotify::look_up_name(
                                WindowHandle::new(self.hwnd),
                                link,
                                self.spotify_names.clone(),
                            );
                        }
                    }
                    NewShortcut::AskName(link) => self.ask_input(InputFor::SpotifyName(link)),
                }
            }
            (InputFor::SpotifyName(link), Some(name)) => {
                self.spotify.add(&link, &name);
            }
        }
        // The pointer has usually moved off the notch to reach the box.
        self.retarget();
        if !self.animating {
            self.render();
        }
    }

    pub(super) fn on_mouse_move(&mut self, wparam: WPARAM, lparam: LPARAM) {
        let held = wparam.0 as u32 & MK_LBUTTON.0 != 0;
        if self.screen_drag {
            if held {
                self.follow_pointer_to_screen();
            } else {
                // The button came up somewhere we didn't hear about.
                self.end_screen_drag(true);
            }
            return;
        }
        if self.drag_armed && held {
            let (x, y) = self.pointer_dips(lparam);
            let (px, py) = self.press_point;
            if (x - px).abs() > DRAG_START_DIPS || (y - py).abs() > DRAG_START_DIPS {
                self.begin_screen_drag();
                return;
            }
        }
        if self.volume_drag {
            if wparam.0 as u32 & MK_LBUTTON.0 != 0 {
                self.set_volume_at(lparam);
            } else {
                // The button came up somewhere we didn't hear about.
                self.end_volume_drag();
            }
            return;
        }
        // Pressing on a Shelf file and moving a few pixels drags it out.
        if let Some(Button::ShelfOpen(index)) = self.pressed
            && wparam.0 as u32 & MK_LBUTTON.0 != 0
        {
            let (x, y) = self.pointer_dips(lparam);
            let (px, py) = self.press_point;
            if (x - px).abs() > DRAG_START_DIPS || (y - py).abs() > DRAG_START_DIPS {
                self.pressed = None;
                self.shelf_drag = self.shelf.items().get(index as usize).cloned();
                WindowHandle::new(self.hwnd).post(msg::SHELF_DRAG, 0, 0);
                return;
            }
        }
        let entered = !self.tracking_mouse;
        if entered {
            let mut track = TRACKMOUSEEVENT {
                cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: self.hwnd,
                dwHoverTime: 0,
            };
            self.tracking_mouse = unsafe { TrackMouseEvent(&mut track) }.is_ok();
        }
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_HOVER_OUT);
            // Started once on entry: Windows also sends WM_MOUSEMOVE while the pointer
            // rests (after region and z-order changes), and re-arming the timer on each
            // one would keep pushing the open back forever.
            if entered && !self.hover {
                SetTimer(Some(self.hwnd), TIMER_HOVER_IN, HOVER_IN_MS, None);
            }
        }
        let hot = self.button_at(lparam);
        if hot != self.hot {
            self.hot = hot;
            if !self.animating {
                self.render();
            }
        }
    }

    pub(super) fn on_mouse_leave(&mut self) {
        self.tracking_mouse = false;
        // Dragging the volume bar or the notch keeps the card open until release.
        if self.volume_drag || self.screen_drag {
            return;
        }
        self.pressed = None;
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_HOVER_IN);
            if self.hover {
                SetTimer(Some(self.hwnd), TIMER_HOVER_OUT, HOVER_OUT_MS, None);
            }
        }
        if self.hot.take().is_some() && !self.animating {
            self.render();
        }
    }

    pub(super) fn set_hover(&mut self, hover: bool) {
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_HOVER_IN);
            let _ = KillTimer(Some(self.hwnd), TIMER_HOVER_OUT);
        }
        if self.hover != hover {
            self.hover = hover;
            // The card is opening: the player may have started or stopped making
            // sound since the last look.
            if hover && self.page == Page::Music {
                self.refresh_app_volume(true);
            }
            self.retarget();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::System::Ole::{DROPEFFECT_COPY, DROPEFFECT_NONE};

    #[test]
    fn a_pop_up_should_not_cut_short_a_notice_over_an_open_card() {
        // (notice showing, new one is a notice, card open) -> new one replaces it
        assert_eq!(
            [
                replaces_hud(true, false, true),
                replaces_hud(true, false, false),
                replaces_hud(true, true, true),
                replaces_hud(false, false, true),
            ],
            [false, true, true, true]
        );
    }

    #[test]
    fn a_pop_up_should_be_dropped_only_while_hidden() {
        assert!(dropped_while_hidden(true, false));
        assert!(!dropped_while_hidden(true, true));
        assert!(!dropped_while_hidden(false, false));
    }

    #[test]
    fn leaving_the_tasks_page_should_close_an_open_task_box() {
        assert_eq!(
            [
                closes_input(Some(Page::Tasks), Page::Water),
                closes_input(Some(Page::Tasks), Page::Shelf),
                closes_input(Some(Page::Tasks), Page::Tasks),
                closes_input(None, Page::Water),
            ],
            [true, true, false, false]
        );
    }

    #[test]
    fn a_drag_should_save_only_the_screen_and_never_defaults() {
        let in_use = Settings {
            screen: r"\\.\DISPLAY2".into(),
            focus_minutes: 50,
            ..Settings::default()
        };
        let file = Settings {
            focus_minutes: 45,
            ..Settings::default()
        };
        let saved = screen_save(Some(file), &in_use);
        assert_eq!(
            saved.as_ref().map(|s| (s.screen.as_str(), s.focus_minutes)),
            Some((r"\\.\DISPLAY2", 45))
        );
        let damaged = screen_save(None, &in_use);
        assert_eq!(damaged.map(|s| s.focus_minutes), Some(50));
        let same = Settings {
            screen: in_use.screen.clone(),
            ..Settings::default()
        };
        assert!(screen_save(Some(same), &in_use).is_none());
    }

    #[test]
    fn a_right_click_should_remove_a_shortcut_even_on_its_x() {
        assert_eq!(
            [
                right_click(Some(Button::SpotifyOpen(2))),
                right_click(Some(Button::SpotifyRemove(2))),
                right_click(Some(Button::SpotifyAdd)),
                right_click(Some(Button::ShelfOpen(0))),
                right_click(None),
            ],
            [
                Some(Button::SpotifyRemove(2)),
                Some(Button::SpotifyRemove(2)),
                None,
                None,
                None,
            ]
        );
    }

    #[test]
    fn a_double_click_should_not_reach_the_next_shortcut() {
        assert!(is_list_click(Some(Button::SpotifyRemove(0))));
        assert!(is_list_click(Some(Button::SpotifyOpen(0))));
        assert!(!is_list_click(Some(Button::SpotifyAdd)));
        assert!(!is_list_click(Some(Button::PlayPause)));
    }

    #[test]
    fn the_spotify_box_should_close_off_its_own_page() {
        assert!(closes_input(Some(Page::Spotify), Page::Tasks));
        assert!(!closes_input(Some(Page::Spotify), Page::Spotify));
    }

    const ID: &str = "37i9dQZF1DXcBWIGoYBM5M";

    #[test]
    fn a_new_link_should_be_named_the_way_the_setting_says() {
        let list = spotify::Shortcuts::default();
        let text = format!("https://open.spotify.com/album/{ID}?si=x");
        let link = spotify::Link::parse(&text);
        assert!(link.is_some());
        let link = link.unwrap_or_else(|| unreachable!());
        assert_eq!(
            new_shortcut(&text, &list, true),
            NewShortcut::AddAndLookUp(link.clone())
        );
        assert_eq!(
            new_shortcut(&text, &list, false),
            NewShortcut::AskName(link)
        );
    }

    #[test]
    fn a_link_already_there_or_a_full_list_should_add_nothing() {
        let text = format!("spotify:playlist:{ID}");
        let there = spotify::Link::parse(&text).into_iter().collect::<Vec<_>>();
        assert_eq!(there.len(), 1);
        let list = spotify::Shortcuts::unnamed(&there);
        assert_eq!(new_shortcut(&text, &list, true), NewShortcut::Refused);
        let others: Vec<spotify::Link> = (0..spotify::MAX_SHORTCUTS)
            .filter_map(|i| spotify::Link::parse(&format!("spotify:album:{i}{}", &ID[1..])))
            .collect();
        assert_eq!(others.len(), spotify::MAX_SHORTCUTS);
        let full = spotify::Shortcuts::unnamed(&others);
        assert_eq!(new_shortcut(&text, &full, false), NewShortcut::Refused);
    }

    #[test]
    fn text_that_is_not_a_spotify_link_should_add_nothing() {
        let list = spotify::Shortcuts::default();
        assert_eq!(
            new_shortcut("milk and eggs", &list, true),
            NewShortcut::Refused
        );
    }

    #[test]
    fn a_drag_that_never_happened_should_keep_the_shelf_item() {
        // An offline drive: the drag couldn't start, and the file can't be seen.
        assert!(!gone_after_drag(None, true, || false));
        assert!(!gone_after_drag(None, false, || false));
    }

    #[test]
    fn a_moved_file_should_leave_the_shelf() {
        assert!(gone_after_drag(Some(DROPEFFECT_MOVE), true, || true));
        assert!(gone_after_drag(Some(DROPEFFECT_MOVE), false, || true));
    }

    #[test]
    fn a_dropped_local_file_that_is_gone_should_leave_the_shelf() {
        // Explorer reports some moves as a plain drop.
        assert!(gone_after_drag(Some(DROPEFFECT_NONE), true, || false));
        assert!(!gone_after_drag(Some(DROPEFFECT_COPY), true, || true));
    }

    #[test]
    fn a_network_file_should_not_be_looked_up_after_a_copy() {
        let looked = std::cell::Cell::new(false);
        let gone = gone_after_drag(Some(DROPEFFECT_COPY), false, || {
            looked.set(true);
            false
        });
        assert_eq!((gone, looked.get()), (false, false));
    }

    #[test]
    fn a_caps_press_should_flip_the_indicator_only_when_caps_lock_is_itself() {
        assert_eq!(
            [
                caps_press_shows(false, false),
                caps_press_shows(false, true),
                caps_press_shows(true, false),
                caps_press_shows(true, true),
            ],
            [Some(true), Some(false), None, None]
        );
    }

    #[test]
    fn lyric_timer_should_keep_short_gaps_and_floor_at_15_ms() {
        assert_eq!(lyric_timer_ms(0), 15);
        assert_eq!(lyric_timer_ms(2_500), 2_500);
    }

    fn lines(times: &[u64]) -> Vec<lyrics::Line> {
        times
            .iter()
            .map(|&time_ms| lyrics::Line {
                time_ms,
                text: "line".into(),
            })
            .collect()
    }

    #[test]
    fn a_line_should_show_early_by_the_lead() {
        // The first line is sung at 19.62 s; a 1 s lead shows it from 18.62 s.
        let lines = lines(&[19_620, 27_500]);
        assert_eq!(
            [18_600, 18_620].map(|position| lyric_schedule(&lines, position, 1_000)),
            [(None, Some(20)), (Some(0), Some(7_880))]
        );
    }

    #[test]
    fn no_lead_should_show_each_line_at_its_time() {
        let lines = lines(&[19_620, 27_500]);
        assert_eq!(lyric_schedule(&lines, 19_620, 0), (Some(0), Some(7_880)));
    }

    #[test]
    fn no_line_should_show_before_the_first_starts() {
        let lines = lines(&[5_000, 10_000]);
        assert_eq!(lyric_schedule(&lines, 1_000, 0), (None, Some(4_000)));
    }

    #[test]
    fn the_latest_started_line_should_show() {
        let lines = lines(&[5_000, 10_000]);
        assert_eq!(lyric_schedule(&lines, 10_000, 0), (Some(1), None));
    }

    #[test]
    fn a_line_shorter_than_the_lead_should_still_show() {
        // Lines at 0 and 0.4 s: with a 1 s lead the first would only have shown
        // before the song began.
        let lines = lines(&[0, 400, 5_000]);
        assert_eq!(
            [0, 199, 200].map(|position| lyric_schedule(&lines, position, 1_000).0),
            [Some(0), Some(0), Some(1)]
        );
    }

    #[test]
    fn a_long_gap_should_keep_the_whole_lead() {
        let lines = lines(&[0, 400, 5_000]);
        assert_eq!(lyric_schedule(&lines, 4_000, 1_000), (Some(2), None));
    }

    #[test]
    fn before_a_shifted_song_reaches_its_lyrics_no_line_should_show() {
        // Lyrics shifted 19 s later, 5 s into the song: 14 s before the file starts.
        let lines = lines(&[500, 4_000]);
        assert_eq!(
            [
                lyric_schedule_at(&lines, -14_000, 1_000),
                lyric_schedule_at(&lines, 2_000, 1_000),
            ],
            [(None, Some(14_000)), (Some(0), Some(1_000))]
        );
    }

    #[test]
    fn the_last_line_should_need_no_timer() {
        let lines = lines(&[19_620, 27_500]);
        assert_eq!(lyric_schedule(&lines, 30_000, 1_000), (Some(1), None));
    }

    #[test]
    fn lyric_timer_should_stay_within_what_windows_waits_for() {
        // SetTimer turns anything above USER_TIMER_MAXIMUM into a 10 ms timer.
        const MAXIMUM: u32 = 0x7FFF_FFFF;
        assert_eq!(
            [
                lyric_timer_ms(u64::from(MAXIMUM)),
                lyric_timer_ms(u64::from(MAXIMUM) + 1),
                lyric_timer_ms(u64::from(u32::MAX) + 1),
                lyric_timer_ms(u64::MAX),
            ],
            [MAXIMUM; 4]
        );
    }
}

#[cfg(test)]
mod prop_tests {
    use super::*;
    use proptest::prelude::*;

    fn lines() -> impl Strategy<Value = Vec<lyrics::Line>> {
        prop::collection::vec(0u64..600_000, 0..40).prop_map(|mut times| {
            times.sort_unstable();
            times
                .into_iter()
                .map(|time_ms| lyrics::Line {
                    time_ms,
                    text: "line".into(),
                })
                .collect()
        })
    }

    proptest! {
        #[test]
        fn no_lead_should_match_the_line_times(lines in lines(), position in 0u64..700_000) {
            let index = lines.partition_point(|line| line.time_ms <= position).checked_sub(1);
            let next = lines.get(index.map_or(0, |i| i + 1)).map(|line| line.time_ms - position);
            prop_assert_eq!(lyric_schedule(&lines, position, 0), (index, next));
        }

        #[test]
        fn the_shown_line_should_never_go_back(lines in lines(), early in 0u32..=3_000, a in 0u64..700_000, b in 0u64..700_000) {
            let (first, second) = (a.min(b), a.max(b));
            prop_assert!(lyric_schedule(&lines, first, early).0 <= lyric_schedule(&lines, second, early).0);
        }

        #[test]
        fn the_timer_should_always_reach_another_line(lines in lines(), early in 0u32..=3_000, position in 0u64..700_000) {
            let (index, next_in) = lyric_schedule(&lines, position, early);
            if let Some(next_in) = next_in {
                prop_assert!(next_in > 0);
                prop_assert_ne!(lyric_schedule(&lines, position + next_in, early).0, index);
            }
        }
    }
}
