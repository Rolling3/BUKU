//! Layout, animation and drawing of the notch.

use super::*;

impl App {
    // ----- layout -----

    pub(super) fn canvas_px(&self) -> (u32, u32) {
        (
            (draw::CANVAS_W * self.scale).ceil() as u32,
            (draw::CANVAS_H * self.scale).ceil() as u32,
        )
    }

    /// Centers the canvas at the top of the chosen screen (the main one unless
    /// the user picked another).
    pub(super) fn place_window(&self) {
        let (w, h) = self.canvas_px();
        unsafe {
            let screens = display::screens();
            let area = display::resolve(&screens, &self.settings.screen).map_or(
                RECT {
                    left: 0,
                    top: 0,
                    right: 1920,
                    bottom: 1080,
                },
                |screen| screen.rect,
            );
            let x = area.left + (area.right - area.left - w as i32) / 2;
            let _ = SetWindowPos(
                self.hwnd,
                Some(HWND_TOPMOST),
                x,
                area.top,
                w as i32,
                h as i32,
                SWP_NOACTIVATE,
            );
        }
    }

    pub(super) fn on_display_changed(&mut self) {
        self.region = None;
        self.place_window();
        let (w, h) = self.canvas_px();
        let resized = self
            .gfx
            .as_mut()
            .map(|gfx| gfx.resize(w, h, 96.0 * self.scale));
        if !matches!(resized, Some(Ok(()))) {
            self.gfx = None;
        }
        self.update_region();
        self.render();
    }

    /// Clips the window to the visible notch so clicks around it reach the windows below.
    pub(super) fn update_region(&mut self) {
        let (left, right, bottom) = draw::shape_bounds(self.width.value, self.height.value);
        let s = self.scale;
        let bounds = (
            (left * s).floor() as i32,
            (right * s).ceil() as i32,
            (bottom * s).ceil() as i32,
        );
        // Setting a region makes Windows recalculate the frame, so skip identical ones.
        if self.region == Some(bounds) {
            return;
        }
        self.region = Some(bounds);
        unsafe {
            let region = CreateRectRgn(bounds.0, 0, bounds.1, bounds.2);
            // The window owns the region after this call.
            SetWindowRgn(self.hwnd, Some(region), false);
        }
    }

    pub(super) fn target_view(&self) -> View {
        // A finished Pomodoro or a water reminder is news: it shows even over an open
        // card, which comes back when the notice ends.
        let notice = matches!(self.hud, Some(Hud::Pomodoro { .. } | Hud::Water { .. }));
        // The card stays open while the task text box is, even with the pointer on the box.
        if (self.hover || self.input.is_some() || self.drag_over) && !notice {
            View::Card
        } else if self.hud.is_some() {
            View::Hud
        } else {
            View::Collapsed
        }
    }

    pub(super) fn size_of(&self, view: View) -> (f32, f32) {
        match view {
            View::Collapsed => (self.collapsed_width(), draw::COLLAPSED_H),
            View::Hud => (
                self.shown_hud
                    .as_ref()
                    .or(self.hud.as_ref())
                    .map_or(draw::COLLAPSED_W, |hud| hud.width()),
                draw::HUD_H,
            ),
            View::Card => (draw::CARD_W, draw::CARD_H),
        }
    }

    pub(super) fn collapsed_width(&self) -> f32 {
        let state = media::lock(&self.media.state);
        if !state.playing {
            return if self.pomodoro.is_running() {
                draw::COLLAPSED_PLAYING_W
            } else {
                draw::COLLAPSED_W
            };
        }
        drop(state);
        match (
            self.settings.lyrics_in_notch,
            self.current_lyric(),
            &self.gfx,
        ) {
            (true, Some(line), Some(gfx)) => {
                let (text_w, _) = gfx.measure(&line, &gfx.fonts.lyric_center);
                // A little slack so rounding never trims the last character.
                (text_w + 2.0 * draw::LYRIC_PADDING + 2.0)
                    .clamp(draw::COLLAPSED_PLAYING_W, draw::CANVAS_W - 40.0)
            }
            _ => draw::COLLAPSED_PLAYING_W,
        }
    }

    // ----- animation -----

    /// Points the animation at whatever the notch should show now.
    pub(super) fn retarget(&mut self) {
        let target = self.target_view();
        if target != self.shown {
            self.content.target = 0.0;
        } else {
            let (w, h) = self.size_of(self.shown);
            self.width.target = w;
            self.height.target = h;
        }
        self.kick();
    }

    pub(super) fn kick(&mut self) {
        if !self.animating {
            self.animating = true;
            self.last_frame = Instant::now();
            WindowHandle::new(self.hwnd).post(msg::FRAME, 0, 0);
        }
    }

    pub(super) fn frame(&mut self) {
        let now = Instant::now();
        let dt = (now - self.last_frame).as_secs_f32();
        self.last_frame = now;

        self.content.step(dt);
        let target = self.target_view();
        if self.shown != target && self.content.value == 0.0 {
            self.shown = target;
            self.shown_hud = if target == View::Hud {
                self.hud.clone()
            } else {
                None
            };
            self.on_view_shown();
        }
        let (w, h) = self.size_of(self.shown);
        self.width.target = w;
        self.height.target = h;
        self.width.step(dt);
        self.height.step(dt);

        let near = (self.width.value - w).abs() < 8.0 && (self.height.value - h).abs() < 8.0;
        if self.shown == target && near {
            self.content.target = 1.0;
        }

        self.update_region();
        self.render();

        let settled = self.shown == target
            && self.width.is_settled()
            && self.height.is_settled()
            && self.content.is_settled();
        if settled {
            self.animating = false;
        } else {
            // Wait for the next composition pass so frames line up with the display.
            unsafe {
                let _ = DwmFlush();
            }
            WindowHandle::new(self.hwnd).post(msg::FRAME, 0, 0);
        }
    }

    /// Starts or stops timers that only matter for a particular view.
    pub(super) fn on_view_shown(&mut self) {
        unsafe {
            // Playing music moves the progress bar; with nothing playing the page
            // shows a clock instead. Either way it changes while the card is open.
            if self.shown == View::Card && self.page == Page::Music {
                SetTimer(Some(self.hwnd), TIMER_PROGRESS, 1000, None);
            } else {
                let _ = KillTimer(Some(self.hwnd), TIMER_PROGRESS);
            }
        }
    }

    pub(super) fn render(&mut self) {
        if self.gfx.is_none() {
            let (w, h) = self.canvas_px();
            self.gfx = Gfx::new(self.hwnd, w, h, 96.0 * self.scale).ok();
            // Bitmaps belong to the device they were made on.
            self.art = None;
            self.shelf_icons.clear();
        }
        self.sync_art();
        if self.shown == View::Card && self.page == Page::Shelf {
            self.sync_shelf_icons();
        }
        let Some(gfx) = &self.gfx else {
            // No usable GPU device right now (driver restarting); try again shortly.
            self.schedule_rebuild(500);
            return;
        };

        let state = media::lock(&self.media.state);
        let lyric = self.current_lyric_in(&state);
        let art = self.art.as_ref().map(|(_, bitmap)| bitmap);
        let now = Instant::now();
        let timer = self
            .pomodoro
            .is_running()
            .then(|| pomodoro::format_remaining(self.pomodoro.remaining(now)));
        let (time, date) = if self.shown == View::Card && self.page == Page::Music {
            clock_strings()
        } else {
            Default::default()
        };
        let focus_left = pomodoro::format_remaining(self.pomodoro.remaining(now));
        let content = match self.shown {
            View::Collapsed if state.playing || timer.is_some() => Content::Collapsed {
                music: state.playing,
                lyric: lyric
                    .as_deref()
                    .filter(|_| state.playing && self.settings.lyrics_in_notch),
                art: art.filter(|_| state.playing),
                timer: timer.as_deref(),
                beat: self.started.elapsed().as_secs_f32() % EQUALIZER_LOOP_SECS,
            },
            View::Collapsed => Content::Empty,
            View::Hud => self.shown_hud.clone().map_or(Content::Empty, Content::Hud),
            View::Card => {
                let body = match (self.page, &state.track) {
                    (Page::Music, Some(track)) => PageBody::Music(MediaCard {
                        title: &track.title,
                        artist: &track.artist,
                        playing: state.playing,
                        lyric: lyric.as_deref(),
                        lyric_offset: matches!(state.lyrics, Some(Lyrics::Synced(_)))
                            .then(|| self.lyric_offset(&state)),
                        position_ms: state.position_ms(),
                        duration_ms: track.duration_ms,
                        art,
                        volume: self
                            .app_level
                            .map(|(level, muted)| (level as f32 / 1000.0, muted)),
                    }),
                    (Page::Music, None) => PageBody::Idle {
                        time: &time,
                        date: &date,
                    },
                    (Page::Focus, _) => PageBody::Focus(FocusCard {
                        remaining: &focus_left,
                        running: self.pomodoro.is_running(),
                        on_break: self.pomodoro.phase == Phase::Break,
                        sessions: self.pomodoro.sessions,
                        progress: self.pomodoro.progress(now),
                    }),
                    (Page::Tasks, _) => {
                        let order = self.tasks.display_order();
                        PageBody::Tasks(TasksCard {
                            rows: order
                                .iter()
                                .take(TASK_ROWS)
                                .filter_map(|&i| self.tasks.get(i))
                                .map(|task| (task.text.as_str(), task.done))
                                .collect(),
                            more: order.len().saturating_sub(TASK_ROWS),
                            any_done: self.tasks.any_done(),
                            can_add: !self.tasks.is_full(),
                        })
                    }
                    (Page::Shelf, _) => PageBody::Shelf(ShelfCard {
                        items: self
                            .shelf
                            .items()
                            .iter()
                            .take(SHELF_TILES)
                            .map(|path| {
                                (
                                    shelf::display_name(path),
                                    self.shelf_icons
                                        .get(path)
                                        .and_then(|(icon, _)| icon.as_ref()),
                                )
                            })
                            .collect(),
                        more: self.shelf.items().len().saturating_sub(SHELF_TILES),
                        dragging: self.drag_over,
                    }),
                    (Page::Water, _) => PageBody::Water(WaterCard {
                        glasses: self.water.glasses(&water::local_now().date),
                        goal: self.settings.water_goal,
                    }),
                };
                Content::Card(Card {
                    page: self.page,
                    hot: self.hot,
                    body,
                })
            }
        };
        let scene = Scene {
            width: self.width.value,
            target_width: self.width.target,
            height: self.height.value,
            content_alpha: crate::anim::ease_out(self.content.value),
            content,
        };
        // The equalizer shows only in the collapsed notch while music plays with
        // no lyric line or countdown beside it; it needs frames only then.
        let equalizer = !self.fullscreen
            && matches!(
                scene.content,
                Content::Collapsed {
                    music: true,
                    lyric: None,
                    timer: None,
                    ..
                }
            );
        draw::render(gfx, &scene);
        drop(state);
        if equalizer != self.equalizer_running {
            self.equalizer_running = equalizer;
            unsafe {
                if equalizer {
                    SetTimer(Some(self.hwnd), TIMER_EQUALIZER, EQUALIZER_FRAME_MS, None);
                } else {
                    let _ = KillTimer(Some(self.hwnd), TIMER_EQUALIZER);
                }
            }
        }

        if gfx.end().is_err() {
            // Device lost (driver update, GPU reset). The notch is blank until rebuilt,
            // so don't wait for the next animation or event to do it.
            self.gfx = None;
            self.schedule_rebuild(16);
        }
    }

    /// Makes the album-art bitmap match the media state.
    fn sync_art(&mut self) {
        let (id, art) = {
            let state = media::lock(&self.media.state);
            (state.art_id, state.art.clone())
        };
        if self.art.as_ref().is_some_and(|(cached, _)| *cached == id) {
            return;
        }
        self.art = match (&self.gfx, art) {
            (Some(gfx), Some(art)) => gfx.create_art_bitmap(&art).ok().map(|bitmap| (id, bitmap)),
            _ => None,
        };
    }

    /// Loads icons for Shelf files that don't have one yet, and forgets the rest.
    fn sync_shelf_icons(&mut self) {
        let Some(gfx) = &self.gfx else { return };
        let items = self.shelf.items();
        self.shelf_icons.retain(|path, _| items.contains(path));
        let now = Instant::now();
        for path in items.iter().take(SHELF_TILES) {
            let known = self
                .shelf_icons
                .get(path)
                .map(|(icon, tried)| (icon.is_some(), now.duration_since(*tried)));
            if !icon_lookup_due(known) {
                continue;
            }
            let bitmap = dragdrop::file_icon(path)
                .and_then(|(side, pixels)| gfx.create_bitmap(side, &pixels).ok());
            self.shelf_icons.insert(path.clone(), (bitmap, now));
        }
    }

    pub(super) fn schedule_rebuild(&self, ms: u32) {
        unsafe {
            SetTimer(Some(self.hwnd), TIMER_REBUILD, ms, None);
        }
    }
}

/// Local time and long date, formatted the way the user's Windows locale does.
fn clock_strings() -> (String, String) {
    let mut time = [0u16; 64];
    let mut date = [0u16; 128];
    unsafe {
        let n = GetTimeFormatEx(None, TIME_NOSECONDS, None, None, Some(&mut time));
        let m = GetDateFormatEx(
            None,
            windows::Win32::Globalization::DATE_LONGDATE,
            None,
            None,
            Some(&mut date),
            None,
        );
        let decode =
            |buf: &[u16], len: i32| String::from_utf16_lossy(&buf[..(len.max(1) - 1) as usize]);
        (decode(&time, n), decode(&date, m))
    }
}

/// How long a Shelf icon that couldn't be loaded waits before another try: its
/// drive may be plugged in later, but a lookup on every frame would be wasted.
const ICON_RETRY: Duration = Duration::from_secs(5);

/// Whether to look up a Shelf file's icon, given whether it has one and how long
/// ago it was tried (`None`: never).
fn icon_lookup_due(known: Option<(bool, Duration)>) -> bool {
    match known {
        None => true,
        Some((found, since)) => !found && since >= ICON_RETRY,
    }
}

#[cfg(test)]
mod shelf_icon_tests {
    use super::*;

    #[test]
    fn an_icon_should_be_looked_up_once_found() {
        assert!(icon_lookup_due(None));
        assert!(!icon_lookup_due(Some((true, Duration::from_secs(600)))));
    }

    #[test]
    fn a_failed_icon_should_be_tried_again_but_not_on_every_frame() {
        // A file on a drive that wasn't plugged in yet.
        assert!(!icon_lookup_due(Some((false, Duration::from_millis(16)))));
        assert!(icon_lookup_due(Some((false, ICON_RETRY))));
    }
}
