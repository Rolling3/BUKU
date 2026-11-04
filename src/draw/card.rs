//! The hover card: Music, Focus and Water pages with a tab strip.
//!
//! Layout and hit-testing share `hits`, so a button is clickable exactly where it's drawn.

use windows::Win32::Graphics::Direct2D::ID2D1Bitmap1;
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_TEXT_ALIGNMENT, DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_LEADING,
    DWRITE_TEXT_ALIGNMENT_TRAILING, IDWriteTextFormat,
};

use super::*;
use crate::lyric_sync;

pub const CARD_W: f32 = 450.0;
pub const CARD_H: f32 = 188.0;

const TAB_Y: f32 = 152.0;
const TAB_H: f32 = 24.0;
const TAB_W: f32 = 72.0;
const TAB_GAP: f32 = 6.0;
const BUTTON_RADIUS: f32 = 18.0;
const BIG_BUTTON_RADIUS: f32 = 22.0;

const WATER: D2D1_COLOR_F = rgba(0.38, 0.72, 1.0, 1.0);
const BREAK: D2D1_COLOR_F = rgba(0.45, 0.85, 0.55, 1.0);

const GLYPH_RESET: &str = "\u{E72C}";
const GLYPH_ADD: &str = "\u{E710}";
const GLYPH_REMOVE: &str = "\u{E738}";
const GLYPH_CHECK: &str = "\u{E73E}";
const GLYPH_CLOSE: &str = "\u{E711}";
const GLYPH_PIN: &str = "\u{E718}";
const GLYPH_PINNED: &str = "\u{E840}";
/// The pin's head, filled: drawn under the outline while the pin is on.
const GLYPH_PINNED_FILL: &str = "\u{E842}";
const GLYPH_CHEVRON_LEFT: &str = "\u{E76B}";
const GLYPH_CHEVRON_RIGHT: &str = "\u{E76C}";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Page {
    #[default]
    Music,
    Focus,
    Water,
    Tasks,
    Shelf,
}

const PAGES: [(Page, &str); 5] = [
    (Page::Music, "Music"),
    (Page::Focus, "Focus"),
    (Page::Water, "Water"),
    (Page::Tasks, "Tasks"),
    (Page::Shelf, "Shelf"),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    Tab(Page),
    /// Top right: keeps the card open when the pointer leaves.
    Pin,
    Previous,
    PlayPause,
    Next,
    /// The speaker icon left of the volume bar: mutes and unmutes.
    Mute,
    /// The volume bar: a press or drag sets the level where the pointer is.
    Volume,
    /// The lyric line: the line shown starts now.
    LyricThisLine,
    /// Left of the lyric line: the line before starts now.
    LyricPrevious,
    /// Right of the lyric line: the line after starts now.
    LyricNext,
    /// The offset under the lyric line: back to the lyrics' own timing.
    LyricReset,
    FocusStartPause,
    FocusReset,
    WaterAdd,
    WaterRemove,
    /// A task row, by its position on the card.
    TaskToggle(u8),
    TaskDelete(u8),
    TaskAdd,
    TaskClearDone,
    /// A Shelf file, by its position on the card.
    ShelfOpen(u8),
    ShelfRemove(u8),
    ShelfClear,
}

pub struct MediaCard<'a> {
    pub title: &'a str,
    pub artist: &'a str,
    pub playing: bool,
    pub lyric: Option<&'a str>,
    /// The song's lyric offset when it has synced lyrics; the timing controls
    /// show only then.
    pub lyric_offset: Option<i64>,
    pub position_ms: u64,
    pub duration_ms: u64,
    pub art: Option<&'a ID2D1Bitmap1>,
    /// System volume (0 to 1) and mute, or `None` with no playback device.
    pub volume: Option<(f32, bool)>,
}

pub struct FocusCard<'a> {
    pub remaining: &'a str,
    pub running: bool,
    pub on_break: bool,
    pub sessions: u32,
    pub progress: f32,
}

/// Rows of the Tasks page shown at once.
pub const TASK_ROWS: usize = 4;

pub struct TasksCard<'a> {
    /// Text and done-state of the visible rows, at most `TASK_ROWS`.
    pub rows: Vec<(&'a str, bool)>,
    /// Tasks that didn't fit.
    pub more: usize,
    pub any_done: bool,
    pub can_add: bool,
}

/// Files shown on the Shelf page at once.
pub const SHELF_TILES: usize = 6;

pub struct ShelfCard<'a> {
    /// Name and icon of the visible files, at most `SHELF_TILES`.
    pub items: Vec<(String, Option<&'a ID2D1Bitmap1>)>,
    pub more: usize,
    /// Files are being dragged over the notch right now.
    pub dragging: bool,
}

pub struct WaterCard {
    pub glasses: u32,
    pub goal: u32,
}

pub enum PageBody<'a> {
    Music(MediaCard<'a>),
    /// The Music page when nothing is playing.
    Idle {
        time: &'a str,
        date: &'a str,
    },
    Focus(FocusCard<'a>),
    Water(WaterCard),
    Tasks(TasksCard<'a>),
    Shelf(ShelfCard<'a>),
}

pub struct Card<'a> {
    pub page: Page,
    pub hot: Option<Button>,
    pub body: PageBody<'a>,
    /// The card stays open until the pin is clicked again.
    pub pinned: bool,
}

/// What each page shows, for hit-testing without building a whole `Card`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageKind {
    Music {
        /// Synced lyrics, which the timing controls line up.
        lyrics: bool,
        /// The song's lyrics are shifted, so the reset shows.
        offset: bool,
    },
    Idle,
    Focus,
    Water,
    /// `done` has bit `i` set when visible row `i` is finished.
    Tasks {
        rows: u8,
        done: u8,
        any_done: bool,
        /// False when the list is full; "Add task" then does nothing.
        can_add: bool,
    },
    Shelf {
        count: u8,
    },
}

enum Hit {
    Circle { cx: f32, cy: f32, r: f32 },
    Rect { x: f32, y: f32, w: f32, h: f32 },
}

impl Hit {
    fn contains(&self, x: f32, y: f32) -> bool {
        match *self {
            Hit::Circle { cx, cy, r } => (x - cx).powi(2) + (y - cy).powi(2) <= r * r,
            Hit::Rect { x: rx, y: ry, w, h } => x >= rx && x <= rx + w && y >= ry && y <= ry + h,
        }
    }
}

fn tabs_left() -> f32 {
    let count = PAGES.len() as f32;
    (CARD_W - (count * TAB_W + (count - 1.0) * TAB_GAP)) / 2.0
}

// The volume row sits under the playback buttons and spans the same width.
const VOLUME_CY: f32 = 74.0;
const VOLUME_ICON_X: f32 = 130.0;
const VOLUME_BAR_LEFT: f32 = 112.0;
const VOLUME_BAR_RIGHT: f32 = 24.0;
const VOLUME_HIT_H: f32 = 16.0;

// The pin, in the card's top right corner: clear of Next, the last Shelf
// tile's remove button and the first task's delete button.
const PIN_X: f32 = 10.0;
const PIN_Y: f32 = 12.0;
const PIN_R: f32 = 8.0;

// The lyric line, with a step button at each end, between the volume row and
// the progress bar; the offset sits between the two clocks under the bar.
const LYRIC_Y: f32 = 82.0;
const LYRIC_H: f32 = 28.0;
const LYRIC_STEP_X: f32 = 32.0;
const LYRIC_STEP_R: f32 = 12.0;
const LYRIC_TEXT_INSET: f32 = 50.0;
const OFFSET_Y: f32 = 127.0;
const OFFSET_W: f32 = 130.0;
const OFFSET_H: f32 = 18.0;

/// Volume (0 to 1) for a pointer at card-relative `x` on a card `width` wide.
fn volume_at(width: f32, x: f32) -> f32 {
    let left = width - VOLUME_BAR_LEFT;
    let right = width - VOLUME_BAR_RIGHT;
    ((x - left) / (right - left)).clamp(0.0, 1.0)
}

/// Volume for a pointer at canvas `x`, measured the way `button_at` measures.
pub fn volume_at_canvas(width: f32, x: f32) -> f32 {
    volume_at(width, x - body_left(width))
}

const TASK_TOP: f32 = 12.0;
const TASK_ROW_H: f32 = 26.0;
const TASK_BUTTON_Y: f32 = 120.0;
const TASK_BUTTON_W: f32 = 104.0;
const TASK_BUTTON_H: f32 = 24.0;

const TILE_W: f32 = 64.0;
const TILE_GAP: f32 = 6.0;
const TILE_Y: f32 = 12.0;
const TILE_H: f32 = 84.0;

fn tile_x(index: usize) -> f32 {
    let row_w = SHELF_TILES as f32 * TILE_W + (SHELF_TILES - 1) as f32 * TILE_GAP;
    (CARD_W - row_w) / 2.0 + index as f32 * (TILE_W + TILE_GAP)
}

fn task_row_y(row: usize) -> f32 {
    TASK_TOP + row as f32 * TASK_ROW_H
}

/// Clickable areas of a page, relative to the card's left edge, on a card `width`
/// wide: right-aligned controls follow the live width the way they're drawn.
fn hits(kind: PageKind, width: f32) -> Vec<(Button, Hit)> {
    let mut hits: Vec<(Button, Hit)> = PAGES
        .iter()
        .enumerate()
        .map(|(i, (page, _))| {
            let x = tabs_left() + i as f32 * (TAB_W + TAB_GAP);
            (
                Button::Tab(*page),
                Hit::Rect {
                    x,
                    y: TAB_Y,
                    w: TAB_W,
                    h: TAB_H,
                },
            )
        })
        .collect();
    hits.push((
        Button::Pin,
        Hit::Circle {
            cx: width - PIN_X,
            cy: PIN_Y,
            r: PIN_R,
        },
    ));
    let circle = |cx: f32, r: f32| Hit::Circle { cx, cy: 50.0, r };
    match kind {
        PageKind::Music { lyrics, offset } => {
            if lyrics {
                let cy = LYRIC_Y + LYRIC_H / 2.0;
                hits.push((
                    Button::LyricPrevious,
                    Hit::Circle {
                        cx: LYRIC_STEP_X,
                        cy,
                        r: LYRIC_STEP_R,
                    },
                ));
                hits.push((
                    Button::LyricNext,
                    Hit::Circle {
                        cx: width - LYRIC_STEP_X,
                        cy,
                        r: LYRIC_STEP_R,
                    },
                ));
                // Starts below the speaker icon's circle, which reaches y 84.
                hits.push((
                    Button::LyricThisLine,
                    Hit::Rect {
                        x: LYRIC_TEXT_INSET,
                        y: LYRIC_Y + 4.0,
                        w: width - 2.0 * LYRIC_TEXT_INSET,
                        h: LYRIC_H - 4.0,
                    },
                ));
            }
            if lyrics && offset {
                hits.push((
                    Button::LyricReset,
                    Hit::Rect {
                        x: (width - OFFSET_W) / 2.0,
                        y: OFFSET_Y,
                        w: OFFSET_W,
                        h: OFFSET_H,
                    },
                ));
            }
            hits.push((
                Button::Previous,
                Hit::Circle {
                    cx: width - 122.0,
                    cy: 44.0,
                    r: BUTTON_RADIUS,
                },
            ));
            hits.push((
                Button::PlayPause,
                Hit::Circle {
                    cx: width - 80.0,
                    cy: 44.0,
                    r: BUTTON_RADIUS,
                },
            ));
            hits.push((
                Button::Next,
                Hit::Circle {
                    cx: width - 38.0,
                    cy: 44.0,
                    r: BUTTON_RADIUS,
                },
            ));
            hits.push((
                Button::Mute,
                Hit::Circle {
                    cx: width - VOLUME_ICON_X,
                    cy: VOLUME_CY,
                    r: 10.0,
                },
            ));
            // A little wider than the bar, so 0 and 100% are easy to reach.
            hits.push((
                Button::Volume,
                Hit::Rect {
                    x: width - VOLUME_BAR_LEFT - 6.0,
                    y: VOLUME_CY - VOLUME_HIT_H / 2.0,
                    w: VOLUME_BAR_LEFT - VOLUME_BAR_RIGHT + 12.0,
                    h: VOLUME_HIT_H,
                },
            ));
        }
        PageKind::Idle => {}
        PageKind::Focus => {
            hits.push((
                Button::FocusStartPause,
                circle(width - 110.0, BIG_BUTTON_RADIUS),
            ));
            hits.push((Button::FocusReset, circle(width - 52.0, BUTTON_RADIUS)));
        }
        PageKind::Water => {
            hits.push((Button::WaterRemove, circle(width - 110.0, BUTTON_RADIUS)));
            hits.push((Button::WaterAdd, circle(width - 52.0, BIG_BUTTON_RADIUS)));
        }
        PageKind::Tasks {
            rows,
            done,
            any_done,
            can_add,
        } => {
            for row in 0..rows.min(TASK_ROWS as u8) {
                let y = task_row_y(row as usize);
                // The whole row toggles, except the delete button on finished tasks.
                let finished = done & (1 << row) != 0;
                let right = if finished { width - 64.0 } else { width - 20.0 };
                hits.push((
                    Button::TaskToggle(row),
                    Hit::Rect {
                        x: 20.0,
                        y,
                        w: right - 20.0,
                        h: TASK_ROW_H,
                    },
                ));
                if finished {
                    hits.push((
                        Button::TaskDelete(row),
                        Hit::Circle {
                            cx: width - 40.0,
                            cy: y + 12.0,
                            r: 11.0,
                        },
                    ));
                }
            }
            let button = |x: f32| Hit::Rect {
                x,
                y: TASK_BUTTON_Y,
                w: TASK_BUTTON_W,
                h: TASK_BUTTON_H,
            };
            if can_add {
                hits.push((Button::TaskAdd, button(20.0)));
            }
            if any_done {
                hits.push((Button::TaskClearDone, button(width - 20.0 - TASK_BUTTON_W)));
            }
        }
        PageKind::Shelf { count } => {
            // The remove buttons sit on the tiles, so they're tested first.
            for index in 0..count.min(SHELF_TILES as u8) {
                let x = tile_x(index as usize);
                hits.push((
                    Button::ShelfRemove(index),
                    Hit::Circle {
                        cx: x + TILE_W - 9.0,
                        cy: TILE_Y + 9.0,
                        r: 9.0,
                    },
                ));
            }
            for index in 0..count.min(SHELF_TILES as u8) {
                let x = tile_x(index as usize);
                hits.push((
                    Button::ShelfOpen(index),
                    Hit::Rect {
                        x,
                        y: TILE_Y,
                        w: TILE_W,
                        h: TILE_H,
                    },
                ));
            }
            if count > 0 {
                hits.push((
                    Button::ShelfClear,
                    Hit::Rect {
                        x: width - 20.0 - TASK_BUTTON_W,
                        y: TASK_BUTTON_Y,
                        w: TASK_BUTTON_W,
                        h: TASK_BUTTON_H,
                    },
                ));
            }
        }
    }
    hits
}

/// The button under (`x`, `y`) in canvas DIPs on a card `width` wide, if any.
pub fn button_at(kind: PageKind, width: f32, x: f32, y: f32) -> Option<Button> {
    let x = x - body_left(width);
    hits(kind, width)
        .into_iter()
        .find(|(_, hit)| hit.contains(x, y))
        .map(|(button, _)| button)
}

impl Painter<'_> {
    pub(super) fn card(&self, card: &Card) {
        match &card.body {
            PageBody::Music(media) => self.music(media, card.hot),
            PageBody::Idle { time, date } => self.idle(time, date),
            PageBody::Focus(focus) => self.focus(focus, card.hot),
            PageBody::Water(water) => self.water(water, card.hot),
            PageBody::Tasks(tasks) => self.tasks(tasks, card.hot),
            PageBody::Shelf(shelf) => self.shelf(shelf, card.hot),
        }
        self.tabs(card.page, card.hot);
        self.pin(card.pinned, card.hot);
    }

    /// The pin: dim while off, in the accent colour while it keeps the card open.
    fn pin(&self, pinned: bool, hot: Option<Button>) {
        let (cx, cy) = (self.width - PIN_X, PIN_Y);
        if hot == Some(Button::Pin) {
            self.circle(cx, cy, PIN_R + 2.0, HOT);
        }
        let (glyph, color) = if pinned {
            (GLYPH_PINNED, accent::current())
        } else if hot == Some(Button::Pin) {
            (GLYPH_PIN, TEXT)
        } else {
            (GLYPH_PIN, TEXT_DIM)
        };
        let r = PIN_R;
        let layers: &[&str] = if pinned {
            &[GLYPH_PINNED_FILL, glyph]
        } else {
            &[glyph]
        };
        for layer in layers {
            self.text(
                layer,
                &self.gfx.fonts.icon_small,
                cx - r,
                cy - r,
                r * 2.0,
                r * 2.0,
                color,
            );
        }
    }

    fn tabs(&self, selected: Page, hot: Option<Button>) {
        let fonts = &self.gfx.fonts;
        for (i, (page, label)) in PAGES.iter().enumerate() {
            let x = tabs_left() + i as f32 * (TAB_W + TAB_GAP);
            let (fill, color) = if *page == selected {
                (rgba(1.0, 1.0, 1.0, 0.16), TEXT)
            } else if hot == Some(Button::Tab(*page)) {
                (HOT, TEXT)
            } else {
                (rgba(0.0, 0.0, 0.0, 0.0), TEXT_DIM)
            };
            self.pill(x, TAB_Y, TAB_W, TAB_H, fill);
            self.aligned(
                label,
                &fonts.small,
                x,
                TAB_Y,
                TAB_W,
                TAB_H,
                color,
                DWRITE_TEXT_ALIGNMENT_CENTER,
            );
        }
    }

    /// A round icon button, highlighted while the pointer is over it.
    #[expect(
        clippy::too_many_arguments,
        reason = "position, size, glyph, style and state of one button"
    )]
    fn round_button(
        &self,
        button: Button,
        hot: Option<Button>,
        cx: f32,
        cy: f32,
        r: f32,
        glyph: &str,
        filled: Option<D2D1_COLOR_F>,
    ) {
        match filled {
            Some(color) => self.circle(cx, cy, r, color),
            None if hot == Some(button) => self.circle(cx, cy, r, HOT),
            None => {}
        }
        let fonts = &self.gfx.fonts;
        let (format, color) = match filled {
            Some(_) => (&fonts.icon_large, BODY),
            None => (&fonts.icon, TEXT),
        };
        self.text(glyph, format, cx - r, cy - r, r * 2.0, r * 2.0, color);
    }

    #[expect(clippy::too_many_arguments, reason = "text, style, box and alignment")]
    fn aligned(
        &self,
        text: &str,
        format: &IDWriteTextFormat,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        color: D2D1_COLOR_F,
        align: DWRITE_TEXT_ALIGNMENT,
    ) {
        unsafe {
            let _ = format.SetTextAlignment(align);
        }
        self.text(text, format, x, y, w, h, color);
        unsafe {
            let _ = format.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_LEADING);
        }
    }

    fn music(&self, card: &MediaCard, hot: Option<Button>) {
        let fonts = &self.gfx.fonts;
        let w = self.width;

        match card.art {
            Some(art) => self.image(art, 20.0, 16.0, 56.0, 12.0),
            None => {
                self.rounded(20.0, 16.0, 56.0, 56.0, 12.0, rgba(1.0, 1.0, 1.0, 0.10));
                self.text(
                    GLYPH_MUSIC,
                    &fonts.icon_large,
                    20.0,
                    16.0,
                    56.0,
                    56.0,
                    accent::current(),
                );
            }
        }

        let text_w = w - 88.0 - 150.0;
        self.text(card.title, &fonts.title, 88.0, 22.0, text_w, 22.0, TEXT);
        self.text(card.artist, &fonts.body, 88.0, 44.0, text_w, 20.0, TEXT_DIM);

        let playback = PageKind::Music {
            lyrics: false,
            offset: false,
        };
        for (button, hit) in hits(playback, self.width) {
            let Hit::Circle { cx, cy, r } = hit else {
                continue;
            };
            let glyph = match button {
                Button::Previous => GLYPH_PREVIOUS,
                Button::Next => GLYPH_NEXT,
                Button::PlayPause if card.playing => GLYPH_PAUSE,
                Button::PlayPause => GLYPH_PLAY,
                // The volume row draws its own.
                _ => continue,
            };
            if hot == Some(button) {
                self.circle(cx, cy, r, HOT);
            }
            let format = if button == Button::PlayPause {
                &fonts.icon_large
            } else {
                &fonts.icon
            };
            self.text(glyph, format, cx - 18.0, cy - 18.0, 36.0, 36.0, TEXT);
        }

        if let Some((level, muted)) = card.volume {
            self.volume_row(level, muted, hot);
        }

        let inset = if card.lyric_offset.is_some() {
            self.lyric_controls(hot, card.lyric.is_some());
            LYRIC_TEXT_INSET
        } else {
            20.0
        };
        if let Some(line) = card.lyric {
            self.text(
                line,
                &fonts.lyric_center,
                inset,
                LYRIC_Y,
                w - 2.0 * inset,
                LYRIC_H,
                accent::current(),
            );
        }

        let bar_w = w - 40.0;
        self.pill(20.0, 120.0, bar_w, 4.0, TRACK);
        if card.duration_ms > 0 {
            let progress = card.position_ms as f32 / card.duration_ms as f32;
            self.pill(20.0, 120.0, bar_w * progress.clamp(0.0, 1.0), 4.0, TEXT);
            let clock = |ms: u64| format!("{}:{:02}", ms / 60_000, ms / 1000 % 60);
            self.aligned(
                &clock(card.position_ms),
                &fonts.small,
                20.0,
                128.0,
                60.0,
                16.0,
                TEXT_DIM,
                DWRITE_TEXT_ALIGNMENT_LEADING,
            );
            self.aligned(
                &clock(card.duration_ms),
                &fonts.small,
                w - 80.0,
                128.0,
                60.0,
                16.0,
                TEXT_DIM,
                DWRITE_TEXT_ALIGNMENT_TRAILING,
            );
        }
        if let Some(offset) = card.lyric_offset.filter(|&offset| offset != 0) {
            let hovered = hot == Some(Button::LyricReset);
            let (label, color) = if hovered {
                ("Reset lyric timing".to_owned(), TEXT)
            } else {
                (lyric_sync::describe(offset), TEXT_DIM)
            };
            let x = (w - OFFSET_W) / 2.0;
            if hovered {
                self.pill(x, OFFSET_Y, OFFSET_W, OFFSET_H, HOT);
            }
            self.aligned(
                &label,
                &fonts.small,
                x,
                OFFSET_Y + 1.0,
                OFFSET_W,
                16.0,
                color,
                DWRITE_TEXT_ALIGNMENT_CENTER,
            );
        }
    }

    /// The step buttons beside the lyric line, and its highlight on hover while a
    /// line shows.
    fn lyric_controls(&self, hot: Option<Button>, line_shown: bool) {
        let w = self.width;
        let cy = LYRIC_Y + LYRIC_H / 2.0;
        if line_shown && hot == Some(Button::LyricThisLine) {
            self.pill(
                LYRIC_TEXT_INSET,
                LYRIC_Y + 2.0,
                w - 2.0 * LYRIC_TEXT_INSET,
                LYRIC_H - 4.0,
                HOT,
            );
        }
        for (button, cx, glyph) in [
            (Button::LyricPrevious, LYRIC_STEP_X, GLYPH_CHEVRON_LEFT),
            (Button::LyricNext, w - LYRIC_STEP_X, GLYPH_CHEVRON_RIGHT),
        ] {
            let color = if hot == Some(button) {
                self.circle(cx, cy, LYRIC_STEP_R, HOT);
                TEXT
            } else {
                TEXT_DIM
            };
            let r = LYRIC_STEP_R;
            self.text(
                glyph,
                &self.gfx.fonts.icon_small,
                cx - r,
                cy - r,
                r * 2.0,
                r * 2.0,
                color,
            );
        }
    }

    /// Speaker icon and volume bar under the playback buttons.
    fn volume_row(&self, level: f32, muted: bool, hot: Option<Button>) {
        let w = self.width;
        let (cx, cy) = (w - VOLUME_ICON_X, VOLUME_CY);
        if hot == Some(Button::Mute) {
            self.circle(cx, cy, 11.0, HOT);
        }
        let color = if muted { TEXT_DIM } else { TEXT };
        let glyph = volume_glyph(level, muted);
        self.text(
            glyph,
            &self.gfx.fonts.icon,
            cx - 12.0,
            cy - 12.0,
            24.0,
            24.0,
            color,
        );

        let left = w - VOLUME_BAR_LEFT;
        let bar_w = VOLUME_BAR_LEFT - VOLUME_BAR_RIGHT;
        self.pill(left, cy - 2.0, bar_w, 4.0, TRACK);
        let filled = bar_w * level.clamp(0.0, 1.0);
        self.pill(left, cy - 2.0, filled, 4.0, color);
        // The knob shows on hover, like the Windows volume slider.
        if hot == Some(Button::Volume) {
            self.circle(left + filled, cy, 6.0, TEXT);
        }
    }

    fn idle(&self, time: &str, date: &str) {
        let fonts = &self.gfx.fonts;
        let w = self.width;
        self.aligned(
            time,
            &fonts.big,
            20.0,
            28.0,
            w - 40.0,
            48.0,
            TEXT,
            DWRITE_TEXT_ALIGNMENT_CENTER,
        );
        self.aligned(
            date,
            &fonts.body,
            20.0,
            78.0,
            w - 40.0,
            20.0,
            TEXT_DIM,
            DWRITE_TEXT_ALIGNMENT_CENTER,
        );
        self.aligned(
            "Nothing playing",
            &fonts.small,
            20.0,
            108.0,
            w - 40.0,
            18.0,
            TEXT_DIM,
            DWRITE_TEXT_ALIGNMENT_CENTER,
        );
    }

    fn focus(&self, card: &FocusCard, hot: Option<Button>) {
        let fonts = &self.gfx.fonts;
        let w = self.width;
        let color = if card.on_break {
            BREAK
        } else {
            accent::current()
        };

        self.text(card.remaining, &fonts.big, 28.0, 26.0, 200.0, 48.0, TEXT);
        let phase = if card.on_break { "Break" } else { "Focus" };
        let sessions = match card.sessions {
            0 => String::new(),
            1 => " · 1 session done".to_owned(),
            n => format!(" · {n} sessions done"),
        };
        self.text(
            &format!("{phase}{sessions}"),
            &fonts.body,
            30.0,
            78.0,
            w - 180.0,
            20.0,
            color,
        );

        let toggle = if card.running {
            GLYPH_PAUSE
        } else {
            GLYPH_PLAY
        };
        self.round_button(
            Button::FocusStartPause,
            hot,
            w - 110.0,
            50.0,
            BIG_BUTTON_RADIUS,
            toggle,
            Some(color),
        );
        self.round_button(
            Button::FocusReset,
            hot,
            w - 52.0,
            50.0,
            BUTTON_RADIUS,
            GLYPH_RESET,
            None,
        );

        let bar_w = w - 40.0;
        self.pill(20.0, 120.0, bar_w, 4.0, TRACK);
        self.pill(
            20.0,
            120.0,
            bar_w * card.progress.clamp(0.0, 1.0),
            4.0,
            color,
        );
    }

    fn tasks(&self, card: &TasksCard, hot: Option<Button>) {
        let fonts = &self.gfx.fonts;
        let w = self.width;
        if card.rows.is_empty() {
            self.aligned(
                "No tasks yet",
                &fonts.body,
                20.0,
                40.0,
                w - 40.0,
                22.0,
                TEXT_DIM,
                DWRITE_TEXT_ALIGNMENT_CENTER,
            );
        }
        for (row, (text, done)) in card.rows.iter().enumerate().take(TASK_ROWS) {
            let y = task_row_y(row);
            let index = row as u8;
            if hot == Some(Button::TaskToggle(index)) {
                self.rounded(20.0, y, w - 40.0, TASK_ROW_H - 2.0, 8.0, HOT);
            }
            let cy = y + 12.0;
            if *done {
                self.circle(34.0, cy, 8.0, accent::current());
                self.text(
                    GLYPH_CHECK,
                    &fonts.icon_small,
                    26.0,
                    cy - 8.0,
                    16.0,
                    16.0,
                    BODY,
                );
            } else {
                self.circle(34.0, cy, 8.0, TEXT_DIM);
                self.circle(34.0, cy, 6.5, BODY);
            }
            let right = if *done { w - 64.0 } else { w - 24.0 };
            self.text(
                text,
                &fonts.body,
                52.0,
                y + 2.0,
                right - 52.0,
                20.0,
                if *done { TEXT_DIM } else { TEXT },
            );
            if *done {
                if hot == Some(Button::TaskDelete(index)) {
                    self.circle(w - 40.0, cy, 11.0, HOT);
                }
                self.text(
                    GLYPH_CLOSE,
                    &fonts.icon_small,
                    w - 50.0,
                    cy - 10.0,
                    20.0,
                    20.0,
                    TEXT_DIM,
                );
            }
        }

        let pill_button = |button: Button, x: f32, label: &str, color: D2D1_COLOR_F| {
            let fill = if hot == Some(button) {
                rgba(1.0, 1.0, 1.0, 0.18)
            } else {
                rgba(1.0, 1.0, 1.0, 0.10)
            };
            self.pill(x, TASK_BUTTON_Y, TASK_BUTTON_W, TASK_BUTTON_H, fill);
            self.aligned(
                label,
                &fonts.small,
                x,
                TASK_BUTTON_Y,
                TASK_BUTTON_W,
                TASK_BUTTON_H,
                color,
                DWRITE_TEXT_ALIGNMENT_CENTER,
            );
        };
        if card.can_add {
            pill_button(Button::TaskAdd, 20.0, "+  Add task", accent::current());
        } else {
            pill_button(Button::TaskAdd, 20.0, "List full", TEXT_DIM);
        }
        if card.any_done {
            pill_button(
                Button::TaskClearDone,
                w - 20.0 - TASK_BUTTON_W,
                "Clear done",
                TEXT_DIM,
            );
        }
        if card.more > 0 {
            self.aligned(
                &format!("+{} more", card.more),
                &fonts.small,
                0.0,
                TASK_BUTTON_Y,
                w,
                TASK_BUTTON_H,
                TEXT_DIM,
                DWRITE_TEXT_ALIGNMENT_CENTER,
            );
        }
    }

    fn shelf(&self, card: &ShelfCard, hot: Option<Button>) {
        let fonts = &self.gfx.fonts;
        let w = self.width;
        if card.dragging {
            self.rounded(
                12.0,
                6.0,
                w - 24.0,
                140.0,
                16.0,
                // The accent, faint, under the files being dropped.
                {
                    let accent = accent::current();
                    rgba(accent.r, accent.g, accent.b, 0.14)
                },
            );
        }
        if card.items.is_empty() {
            let hint = if card.dragging {
                "Drop to keep them here"
            } else {
                "Drag files onto the notch to keep them here"
            };
            self.aligned(
                hint,
                &fonts.body,
                20.0,
                50.0,
                w - 40.0,
                22.0,
                if card.dragging {
                    accent::current()
                } else {
                    TEXT_DIM
                },
                DWRITE_TEXT_ALIGNMENT_CENTER,
            );
            return;
        }
        for (index, (name, icon)) in card.items.iter().enumerate().take(SHELF_TILES) {
            let x = tile_x(index);
            let tile = index as u8;
            let tile_hot =
                matches!(hot, Some(Button::ShelfOpen(i) | Button::ShelfRemove(i)) if i == tile);
            if tile_hot {
                self.rounded(x, TILE_Y, TILE_W, TILE_H, 12.0, HOT);
            }
            match icon {
                Some(icon) => {
                    self.image_sized(icon, x + (TILE_W - 36.0) / 2.0, TILE_Y + 10.0, 36.0)
                }
                None => self.rounded(x + 14.0, TILE_Y + 10.0, 36.0, 36.0, 8.0, TRACK),
            }
            self.aligned(
                name,
                &fonts.small,
                x + 2.0,
                TILE_Y + 54.0,
                TILE_W - 4.0,
                18.0,
                TEXT,
                DWRITE_TEXT_ALIGNMENT_CENTER,
            );
            if tile_hot {
                let remove_hot = hot == Some(Button::ShelfRemove(tile));
                self.circle(
                    x + TILE_W - 9.0,
                    TILE_Y + 9.0,
                    9.0,
                    if remove_hot {
                        rgba(1.0, 1.0, 1.0, 0.30)
                    } else {
                        rgba(1.0, 1.0, 1.0, 0.16)
                    },
                );
                self.text(
                    GLYPH_CLOSE,
                    &fonts.icon_small,
                    x + TILE_W - 18.0,
                    TILE_Y,
                    18.0,
                    18.0,
                    TEXT,
                );
            }
        }
        let fill = if hot == Some(Button::ShelfClear) {
            rgba(1.0, 1.0, 1.0, 0.18)
        } else {
            rgba(1.0, 1.0, 1.0, 0.10)
        };
        let x = w - 20.0 - TASK_BUTTON_W;
        self.pill(x, TASK_BUTTON_Y, TASK_BUTTON_W, TASK_BUTTON_H, fill);
        self.aligned(
            "Clear",
            &fonts.small,
            x,
            TASK_BUTTON_Y,
            TASK_BUTTON_W,
            TASK_BUTTON_H,
            TEXT_DIM,
            DWRITE_TEXT_ALIGNMENT_CENTER,
        );
        if card.more > 0 {
            self.aligned(
                &format!("+{} more", card.more),
                &fonts.small,
                20.0,
                TASK_BUTTON_Y,
                120.0,
                TASK_BUTTON_H,
                TEXT_DIM,
                DWRITE_TEXT_ALIGNMENT_LEADING,
            );
        }
    }

    fn water(&self, card: &WaterCard, hot: Option<Button>) {
        let fonts = &self.gfx.fonts;
        let w = self.width;

        self.text(
            &format!("{} / {}", card.glasses, card.goal),
            &fonts.big,
            28.0,
            26.0,
            200.0,
            48.0,
            TEXT,
        );
        let label = if card.glasses >= card.goal {
            "Goal reached today"
        } else {
            "glasses of water today"
        };
        self.text(label, &fonts.body, 30.0, 78.0, w - 180.0, 20.0, WATER);

        self.round_button(
            Button::WaterRemove,
            hot,
            w - 110.0,
            50.0,
            BUTTON_RADIUS,
            GLYPH_REMOVE,
            None,
        );
        self.round_button(
            Button::WaterAdd,
            hot,
            w - 52.0,
            50.0,
            BIG_BUTTON_RADIUS,
            GLYPH_ADD,
            Some(WATER),
        );

        let bar_w = w - 40.0;
        let progress = card.glasses as f32 / card.goal.max(1) as f32;
        self.pill(20.0, 120.0, bar_w, 4.0, TRACK);
        self.pill(20.0, 120.0, bar_w * progress.clamp(0.0, 1.0), 4.0, WATER);
    }
}

#[cfg(test)]
mod edge_case_tests {
    use super::*;

    #[test]
    fn volume_at_should_map_the_bar_to_zero_through_one() {
        let w = CARD_W;
        let ends = [
            volume_at(w, w - VOLUME_BAR_LEFT),
            volume_at(w, w - VOLUME_BAR_RIGHT),
            volume_at(w, w - (VOLUME_BAR_LEFT + VOLUME_BAR_RIGHT) / 2.0),
            volume_at(w, 0.0),
            volume_at(w, w),
        ];
        assert_eq!(ends, [0.0, 1.0, 0.5, 0.0, 1.0]);
    }

    #[test]
    fn the_volume_row_should_not_take_clicks_from_the_playback_buttons() {
        let w = CARD_W;
        let kind = PageKind::Music {
            lyrics: false,
            offset: false,
        };
        let at = |x: f32, y: f32| {
            hits(kind, w)
                .into_iter()
                .find(|(_, hit)| hit.contains(x, y))
                .map(|(b, _)| b)
        };
        assert_eq!(
            [
                at(w - 80.0, 44.0),
                at(w - 80.0, VOLUME_CY),
                at(w - VOLUME_ICON_X, VOLUME_CY),
                at(w - 80.0, 90.0),
            ],
            [
                Some(Button::PlayPause),
                Some(Button::Volume),
                Some(Button::Mute),
                None
            ]
        );
    }

    fn music_at(lyrics: bool, offset: bool, x: f32, y: f32) -> Option<Button> {
        hits(PageKind::Music { lyrics, offset }, CARD_W)
            .into_iter()
            .find(|(_, hit)| hit.contains(x, y))
            .map(|(button, _)| button)
    }

    #[test]
    fn the_lyric_controls_should_sit_on_the_lyric_line() {
        let w = CARD_W;
        let cy = LYRIC_Y + LYRIC_H / 2.0;
        assert_eq!(
            [
                music_at(true, false, LYRIC_STEP_X, cy),
                music_at(true, false, w / 2.0, cy),
                music_at(true, false, w - LYRIC_STEP_X, cy),
            ],
            [
                Some(Button::LyricPrevious),
                Some(Button::LyricThisLine),
                Some(Button::LyricNext)
            ]
        );
    }

    #[test]
    fn the_lyric_controls_should_not_take_clicks_from_the_volume_row() {
        let w = CARD_W;
        assert_eq!(
            [
                music_at(true, true, w - VOLUME_ICON_X, VOLUME_CY + 9.0),
                music_at(true, true, w - 60.0, VOLUME_CY + VOLUME_HIT_H / 2.0),
                music_at(true, true, w - 80.0, 44.0),
            ],
            [
                Some(Button::Mute),
                Some(Button::Volume),
                Some(Button::PlayPause)
            ]
        );
    }

    #[test]
    fn without_synced_lyrics_there_should_be_no_lyric_controls() {
        let cy = LYRIC_Y + LYRIC_H / 2.0;
        assert_eq!(
            [
                music_at(false, true, LYRIC_STEP_X, cy),
                music_at(false, true, CARD_W / 2.0, cy),
                music_at(false, true, CARD_W / 2.0, OFFSET_Y + 8.0),
            ],
            [None, None, None]
        );
    }

    #[test]
    fn the_reset_should_show_only_while_the_lyrics_are_shifted() {
        let (x, y) = (CARD_W / 2.0, OFFSET_Y + 8.0);
        assert_eq!(
            [music_at(true, false, x, y), music_at(true, true, x, y)],
            [None, Some(Button::LyricReset)]
        );
    }

    #[test]
    fn a_button_should_be_clickable_where_it_is_drawn_while_the_card_settles() {
        // The card is a few DIPs short of its final width for a few frames.
        let width = CARD_W - 7.5;
        let drawn_center = body_left(width) + width - 110.0;
        // The edge away from the right-hand side, where the offset shows.
        let x = drawn_center - BIG_BUTTON_RADIUS + 1.0;
        assert_eq!(
            button_at(PageKind::Focus, width, x, 50.0),
            Some(Button::FocusStartPause)
        );
    }
}

#[cfg(test)]
mod pin_tests {
    use super::*;

    fn at(kind: PageKind, x: f32, y: f32) -> Option<Button> {
        hits(kind, CARD_W)
            .into_iter()
            .find(|(_, hit)| hit.contains(x, y))
            .map(|(button, _)| button)
    }

    const KINDS: [PageKind; 6] = [
        PageKind::Music {
            lyrics: true,
            offset: true,
        },
        PageKind::Idle,
        PageKind::Focus,
        PageKind::Water,
        PageKind::Tasks {
            rows: 4,
            done: 0b1111,
            any_done: true,
            can_add: true,
        },
        PageKind::Shelf { count: 6 },
    ];

    #[test]
    fn the_pin_should_be_in_the_top_right_corner_of_every_page() {
        let (x, y) = (CARD_W - PIN_X, PIN_Y);
        assert!(
            KINDS
                .iter()
                .all(|&kind| at(kind, x, y) == Some(Button::Pin))
        );
    }

    #[test]
    fn the_pin_should_not_take_clicks_from_the_buttons_near_it() {
        let w = CARD_W;
        assert_eq!(
            [
                at(KINDS[0], w - 38.0, 44.0),
                at(KINDS[4], w - 40.0, 24.0),
                at(
                    KINDS[5],
                    tile_x(SHELF_TILES - 1) + TILE_W - 9.0,
                    TILE_Y + 9.0
                ),
                at(KINDS[2], w - 52.0, 50.0),
            ],
            [
                Some(Button::Next),
                Some(Button::TaskDelete(0)),
                Some(Button::ShelfRemove(SHELF_TILES as u8 - 1)),
                Some(Button::FocusReset),
            ]
        );
    }
}
