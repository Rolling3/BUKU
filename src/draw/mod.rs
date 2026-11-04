//! Drawing the notch. Everything here is in DIPs; `Gfx` maps them to pixels.
//!
//! This module draws the shape, the collapsed notch and the HUDs; `card` draws
//! the hover card and owns its hit-testing.

mod card;

pub use card::{
    Button, CARD_H, CARD_W, Card, FocusCard, MediaCard, Page, PageBody, PageKind, SHELF_TILES,
    ShelfCard, TASK_ROWS, TasksCard, WaterCard, button_at, volume_at_canvas,
};

use windows::Win32::Graphics::Direct2D::Common::{
    D2D_RECT_F, D2D_SIZE_F, D2D1_COLOR_F, D2D1_FIGURE_BEGIN_FILLED, D2D1_FIGURE_END_CLOSED,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1_ARC_SEGMENT, D2D1_ARC_SIZE_SMALL,
    D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_ROUNDED_RECT, D2D1_SWEEP_DIRECTION,
    D2D1_SWEEP_DIRECTION_CLOCKWISE, D2D1_SWEEP_DIRECTION_COUNTER_CLOCKWISE, ID2D1Bitmap1,
    ID2D1GeometrySink,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_MEASURING_MODE_NATURAL, DWRITE_READING_DIRECTION_LEFT_TO_RIGHT,
    DWRITE_READING_DIRECTION_RIGHT_TO_LEFT, DWRITE_TEXT_ALIGNMENT_LEADING,
    DWRITE_TEXT_ALIGNMENT_TRAILING, IDWriteTextFormat,
};
use windows_numerics::{Matrix3x2, Vector2};

use crate::accent;
use crate::art::ART_PX;
use crate::gfx::{Gfx, rgba};

/// Size of the window's drawing surface. The notch shape is centered inside it.
pub const CANVAS_W: f32 = 600.0;
pub const CANVAS_H: f32 = 200.0;
/// Radius of the concave fillets joining the notch to the top edge of the screen.
const EAR: f32 = 9.0;

const BODY: D2D1_COLOR_F = rgba(0.0, 0.0, 0.0, 1.0);
const TEXT: D2D1_COLOR_F = rgba(1.0, 1.0, 1.0, 0.95);
const TEXT_DIM: D2D1_COLOR_F = rgba(1.0, 1.0, 1.0, 0.55);
const TRACK: D2D1_COLOR_F = rgba(1.0, 1.0, 1.0, 0.16);
const HOT: D2D1_COLOR_F = rgba(1.0, 1.0, 1.0, 0.12);

// Segoe Fluent Icons / Segoe MDL2 Assets code points.
const GLYPH_MUSIC: &str = "\u{E8D6}";
const GLYPH_PLAY: &str = "\u{E768}";
const GLYPH_PAUSE: &str = "\u{E769}";
const GLYPH_PREVIOUS: &str = "\u{E892}";
const GLYPH_NEXT: &str = "\u{E893}";
const GLYPH_VOLUME: [&str; 4] = ["\u{E992}", "\u{E993}", "\u{E994}", "\u{E995}"];
const GLYPH_MUTE: &str = "\u{E74F}";
/// Shortest and tallest equalizer bar, in DIPs.
const EQUALIZER_MIN: f32 = 4.0;
const EQUALIZER_MAX: f32 = 14.0;

/// Heights of the three equalizer bars at `beat` seconds. Each bar mixes two
/// waves at speeds that never line up, so the motion doesn't visibly repeat.
fn equalizer_heights(beat: f32) -> [f32; 3] {
    const WAVES: [(f32, f32, f32); 3] = [(7.1, 11.3, 0.0), (9.7, 5.3, 1.9), (6.2, 13.1, 3.7)];
    WAVES.map(|(fast, slow, offset)| {
        let wave = (beat * fast + offset).sin() * 0.6 + (beat * slow + offset * 2.0).sin() * 0.4;
        EQUALIZER_MIN + (EQUALIZER_MAX - EQUALIZER_MIN) * (wave * 0.5 + 0.5)
    })
}

/// The speaker icon for a volume level (0 to 1): more waves as it rises.
fn volume_glyph(level: f32, muted: bool) -> &'static str {
    if muted || level <= 0.0 {
        GLYPH_MUTE
    } else {
        GLYPH_VOLUME[((level * 3.0).ceil() as usize).min(3)]
    }
}

const GLYPH_BRIGHTNESS: &str = "\u{E706}";
const GLYPH_KEYBOARD: &str = "\u{E765}";
const GLYPH_TIMER: &str = "\u{E916}";
const GLYPH_WIFI: &str = "\u{E701}";
const GLYPH_ETHERNET: &str = "\u{E839}";
const GLYPH_HEADPHONES: &str = "\u{E7F6}";
const GLYPH_SPEAKER: &str = "\u{E7F5}";
const GLYPH_LANGUAGE: &str = "\u{E775}";
const GLYPH_BATTERY: [&str; 11] = [
    "\u{E850}", "\u{E851}", "\u{E852}", "\u{E853}", "\u{E854}", "\u{E855}", "\u{E856}", "\u{E857}",
    "\u{E858}", "\u{E859}", "\u{E83F}",
];
const GLYPH_BATTERY_CHARGING: [&str; 11] = [
    "\u{E85A}", "\u{E85B}", "\u{E85C}", "\u{E85D}", "\u{E85E}", "\u{E85F}", "\u{E860}", "\u{E861}",
    "\u{E862}", "\u{E83E}", "\u{EA93}",
];

#[derive(Clone, Debug, PartialEq)]
pub enum Hud {
    Volume {
        level: f32,
        muted: bool,
    },
    Brightness(f32),
    CapsLock(bool),
    /// A Pomodoro phase ended; `focus_done` is true when the break starts.
    Pomodoro {
        focus_done: bool,
    },
    Water {
        glasses: u32,
        goal: u32,
    },
    /// Plugged in or unplugged (`low` when the charge crossed a warning level).
    Power {
        charging: bool,
        percent: Option<u32>,
        low: bool,
    },
    Network {
        connected: bool,
        wifi: bool,
        name: String,
    },
    /// The default playback device changed.
    Output {
        name: String,
        worn: bool,
    },
    /// The keyboard language changed; the language's own name.
    Language(String),
}

impl Hud {
    /// Width of the notch while this HUD shows.
    pub fn width(&self) -> f32 {
        match self {
            Hud::Volume { .. } | Hud::Brightness(_) => HUD_W,
            Hud::CapsLock(_) => CAPS_W,
            Hud::Language(_) => CAPS_W,
            Hud::Pomodoro { .. }
            | Hud::Water { .. }
            | Hud::Power { .. }
            | Hud::Network { .. }
            | Hud::Output { .. } => NOTICE_W,
        }
    }
}

pub enum Content<'a> {
    Empty,
    /// The collapsed notch while music plays or a Pomodoro runs.
    Collapsed {
        music: bool,
        lyric: Option<&'a str>,
        art: Option<&'a ID2D1Bitmap1>,
        timer: Option<&'a str>,
        /// Seconds on the equalizer's clock; its bars move with it.
        beat: f32,
    },
    Hud(Hud),
    Card(Card<'a>),
}

pub struct Scene<'a> {
    pub width: f32,
    /// The width the notch is animating toward. A new lyric line is laid out at
    /// this width and revealed as the notch grows, so it shows on time.
    pub target_width: f32,
    pub height: f32,
    /// Opacity of the content, 0..1. The black body itself is always opaque.
    pub content_alpha: f32,
    pub content: Content<'a>,
}

pub const COLLAPSED_H: f32 = 30.0;
pub const COLLAPSED_W: f32 = 190.0;
pub const COLLAPSED_PLAYING_W: f32 = 230.0;
pub const HUD_H: f32 = 48.0;
const HUD_W: f32 = 320.0;
const CAPS_W: f32 = 250.0;
const NOTICE_W: f32 = 380.0;
/// Space either side of a lyric line in the collapsed notch.
pub const LYRIC_PADDING: f32 = 46.0;

/// Left edge of a notch body of `width`, centered on the canvas.
pub fn body_left(width: f32) -> f32 {
    (CANVAS_W - width) / 2.0
}

/// Bounds of the visible shape, including the top fillets, as (left, right, bottom).
pub fn shape_bounds(width: f32, height: f32) -> (f32, f32, f32) {
    let left = body_left(width);
    (left - EAR, left + width + EAR, height)
}

fn corner_radius(height: f32) -> f32 {
    (height * 0.42).clamp(10.0, 26.0)
}

pub fn render(gfx: &Gfx, scene: &Scene) {
    gfx.begin();
    draw_body(gfx, scene.width, scene.height);
    let alpha = scene.content_alpha;
    if alpha > 0.0 {
        let painter = Painter {
            gfx,
            target_width: scene.target_width,
            left: body_left(scene.width),
            width: scene.width,
            height: scene.height,
            alpha,
        };
        match &scene.content {
            Content::Empty => {}
            Content::Collapsed {
                music,
                lyric,
                art,
                timer,
                beat,
            } => painter.collapsed(*music, *lyric, *art, *timer, *beat),
            Content::Hud(hud) => painter.hud(hud),
            Content::Card(card) => painter.card(card),
        }
    }
}

fn draw_body(gfx: &Gfx, width: f32, height: f32) {
    let Ok(path) = (unsafe { gfx.factory.CreatePathGeometry() }) else {
        return;
    };
    let Ok(sink) = (unsafe { path.Open() }) else {
        return;
    };
    let left = body_left(width);
    let right = left + width;
    let ear = EAR.min(height / 2.0);
    let radius = corner_radius(height)
        .min((height - ear).max(0.0))
        .min(width / 2.0);

    let arc = |sink: &ID2D1GeometrySink, x: f32, y: f32, r: f32, sweep: D2D1_SWEEP_DIRECTION| unsafe {
        sink.AddArc(&D2D1_ARC_SEGMENT {
            point: point(x, y),
            size: D2D_SIZE_F {
                width: r,
                height: r,
            },
            rotationAngle: 0.0,
            sweepDirection: sweep,
            arcSize: D2D1_ARC_SIZE_SMALL,
        });
    };
    unsafe {
        sink.BeginFigure(point(left - ear, 0.0), D2D1_FIGURE_BEGIN_FILLED);
        arc(&sink, left, ear, ear, D2D1_SWEEP_DIRECTION_CLOCKWISE);
        sink.AddLine(point(left, height - radius));
        arc(
            &sink,
            left + radius,
            height,
            radius,
            D2D1_SWEEP_DIRECTION_COUNTER_CLOCKWISE,
        );
        sink.AddLine(point(right - radius, height));
        arc(
            &sink,
            right,
            height - radius,
            radius,
            D2D1_SWEEP_DIRECTION_COUNTER_CLOCKWISE,
        );
        sink.AddLine(point(right, ear));
        arc(&sink, right + ear, 0.0, ear, D2D1_SWEEP_DIRECTION_CLOCKWISE);
        sink.EndFigure(D2D1_FIGURE_END_CLOSED);
        if sink.Close().is_err() {
            return;
        }
        gfx.brush.SetColor(&BODY);
        gfx.ctx.FillGeometry(&path, &gfx.brush, None);
    }
}

struct Painter<'a> {
    gfx: &'a Gfx,
    target_width: f32,
    left: f32,
    width: f32,
    height: f32,
    alpha: f32,
}

impl Painter<'_> {
    fn color(&self, color: D2D1_COLOR_F) {
        unsafe {
            self.gfx
                .brush
                .SetColor(&rgba(color.r, color.g, color.b, color.a * self.alpha));
        }
    }

    /// Draws text in a box given relative to the notch body's left edge.
    #[expect(
        clippy::too_many_arguments,
        reason = "a text box is naturally text, style, four edges and a color"
    )]
    fn text(
        &self,
        text: &str,
        format: &IDWriteTextFormat,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        color: D2D1_COLOR_F,
    ) {
        if text.is_empty() || w <= 0.0 {
            return;
        }
        let wide: Vec<u16> = text.encode_utf16().collect();
        self.color(color);
        let rtl = is_rtl(text);
        unsafe {
            // Arabic and Hebrew titles and lyrics read right to left: the ellipsis and
            // punctuation belong on the left, and leading alignment means the right edge.
            if rtl {
                let _ = format.SetReadingDirection(DWRITE_READING_DIRECTION_RIGHT_TO_LEFT);
            }
            self.gfx.ctx.DrawText(
                &wide,
                format,
                &rect(self.left + x, y, w, h),
                &self.gfx.brush,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
                DWRITE_MEASURING_MODE_NATURAL,
            );
            if rtl {
                let _ = format.SetReadingDirection(DWRITE_READING_DIRECTION_LEFT_TO_RIGHT);
            }
        }
    }

    fn rounded(&self, x: f32, y: f32, w: f32, h: f32, r: f32, color: D2D1_COLOR_F) {
        if w <= 0.0 {
            return;
        }
        self.color(color);
        unsafe {
            self.gfx.ctx.FillRoundedRectangle(
                &D2D1_ROUNDED_RECT {
                    rect: rect(self.left + x, y, w, h),
                    radiusX: r,
                    radiusY: r,
                },
                &self.gfx.brush,
            );
        }
    }

    fn pill(&self, x: f32, y: f32, w: f32, h: f32, color: D2D1_COLOR_F) {
        self.rounded(x, y, w, h, h / 2.0, color);
    }

    fn circle(&self, cx: f32, cy: f32, r: f32, color: D2D1_COLOR_F) {
        self.pill(cx - r, cy - r, r * 2.0, r * 2.0, color);
    }

    /// Draws a square bitmap (a file icon) scaled to `size`, no rounding.
    fn image_sized(&self, bitmap: &ID2D1Bitmap1, x: f32, y: f32, size: f32) {
        unsafe {
            self.gfx.ctx.DrawBitmap(
                bitmap,
                Some(&rect(self.left + x, y, size, size)),
                self.alpha,
                windows::Win32::Graphics::Direct2D::D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC,
                None,
                None,
            );
        }
    }

    /// Draws album art scaled into a `size` square with rounded corners.
    /// The cover is decoded larger than any place it's drawn, so it's filtered
    /// down with the high-quality cubic filter: the default bilinear one skips
    /// source pixels at these ratios and leaves the 18 DIP thumbnail jagged.
    fn image(&self, bitmap: &ID2D1Bitmap1, x: f32, y: f32, size: f32, radius: f32) {
        use windows::Win32::Graphics::Direct2D::{
            D2D1_BITMAP_BRUSH_PROPERTIES1, D2D1_EXTEND_MODE_CLAMP,
            D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC,
        };
        let properties = D2D1_BITMAP_BRUSH_PROPERTIES1 {
            extendModeX: D2D1_EXTEND_MODE_CLAMP,
            extendModeY: D2D1_EXTEND_MODE_CLAMP,
            interpolationMode: D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC,
        };
        let brush = unsafe {
            self.gfx
                .ctx
                .CreateBitmapBrush(bitmap, Some(&properties), None)
        };
        let Ok(brush) = brush else {
            return;
        };
        let scale = size / ART_PX as f32;
        unsafe {
            brush.SetTransform(&Matrix3x2 {
                M11: scale,
                M12: 0.0,
                M21: 0.0,
                M22: scale,
                M31: self.left + x,
                M32: y,
            });
            brush.SetOpacity(self.alpha);
            self.gfx.ctx.FillRoundedRectangle(
                &D2D1_ROUNDED_RECT {
                    rect: rect(self.left + x, y, size, size),
                    radiusX: radius,
                    radiusY: radius,
                },
                &brush,
            );
        }
    }

    fn collapsed(
        &self,
        music: bool,
        lyric: Option<&str>,
        art: Option<&ID2D1Bitmap1>,
        timer: Option<&str>,
        beat: f32,
    ) {
        let fonts = &self.gfx.fonts;
        let mid = self.height / 2.0 - 1.0;
        match (music, art) {
            (true, Some(art)) => self.image(art, 12.0, mid - 9.0, 18.0, 5.0),
            (true, None) => self.text(
                GLYPH_MUSIC,
                &fonts.icon,
                10.0,
                mid - 10.0,
                24.0,
                20.0,
                accent::current(),
            ),
            (false, _) => self.text(
                GLYPH_TIMER,
                &fonts.icon,
                10.0,
                mid - 10.0,
                24.0,
                20.0,
                accent::current(),
            ),
        }
        if let Some(line) = lyric {
            // Laid out at the final width (so it's never cut short with an ellipsis
            // mid-animation) and clipped to the body as it is now.
            let full = self.width.max(self.target_width);
            let w = full - 2.0 * LYRIC_PADDING;
            let x = (self.width - full) / 2.0 + LYRIC_PADDING;
            let clip = rect(
                self.left + 34.0,
                0.0,
                (self.width - 68.0).max(0.0),
                self.height,
            );
            unsafe {
                self.gfx
                    .ctx
                    .PushAxisAlignedClip(&clip, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
            }
            self.text(line, &fonts.lyric_center, x, mid - 11.0, w, 22.0, TEXT);
            unsafe {
                self.gfx.ctx.PopAxisAlignedClip();
            }
        }
        match timer {
            Some(left) => self.text(
                left,
                &fonts.small,
                self.width - 42.0,
                mid - 9.0,
                36.0,
                18.0,
                TEXT,
            ),
            None if lyric.is_none() => {
                // A small equalizer; the app redraws it a few dozen times a second,
                // only while it is on screen.
                for (i, h) in equalizer_heights(beat).into_iter().enumerate() {
                    let x = self.width - 30.0 + i as f32 * 6.0;
                    self.pill(x, mid - h / 2.0, 3.0, h, accent::current());
                }
            }
            None => {}
        }
    }

    fn hud(&self, hud: &Hud) {
        let fonts = &self.gfx.fonts;
        let mid = self.height / 2.0;
        let (glyph, value) = match *hud {
            Hud::Volume { level, muted } => {
                (volume_glyph(level, muted), if muted { 0.0 } else { level })
            }
            Hud::Brightness(level) => (GLYPH_BRIGHTNESS, level),
            Hud::CapsLock(on) => {
                let label = if on { "Caps Lock on" } else { "Caps Lock off" };
                let color = if on { accent::current() } else { TEXT_DIM };
                self.notice(GLYPH_KEYBOARD, label, color);
                return;
            }
            Hud::Pomodoro { focus_done } => {
                let label = if focus_done {
                    "Focus session done. Take a break"
                } else {
                    "Break's over. Ready to focus?"
                };
                self.notice(GLYPH_TIMER, label, accent::current());
                return;
            }
            Hud::Water { glasses, goal } => {
                self.notice(
                    "",
                    &format!("Time for some water  ·  {glasses} of {goal} today"),
                    TEXT,
                );
                self.circle(28.0, mid, 7.0, rgba(0.38, 0.72, 1.0, 1.0));
                return;
            }
            Hud::Power {
                charging,
                percent,
                low,
            } => {
                let level = percent.unwrap_or(100);
                let step = (level as usize + 5) / 10;
                let glyph = if charging {
                    GLYPH_BATTERY_CHARGING[step.min(10)]
                } else {
                    GLYPH_BATTERY[step.min(10)]
                };
                let state = match (charging, low) {
                    (true, _) => "Charging",
                    (false, true) => "Battery low",
                    (false, false) => "On battery",
                };
                let label = match percent {
                    Some(p) => format!("{state}  ·  {p}%"),
                    None => state.to_owned(),
                };
                let color = if low {
                    rgba(1.0, 0.42, 0.38, 1.0)
                } else if charging {
                    accent::current()
                } else {
                    TEXT
                };
                self.notice(glyph, &label, color);
                return;
            }
            Hud::Network {
                connected,
                wifi,
                ref name,
            } => {
                let glyph = if wifi || !connected {
                    GLYPH_WIFI
                } else {
                    GLYPH_ETHERNET
                };
                if connected {
                    let label = if name.is_empty() {
                        "Connected".to_owned()
                    } else {
                        format!("Connected to {name}")
                    };
                    self.notice(glyph, &label, TEXT);
                } else {
                    self.notice(glyph, "Offline", TEXT_DIM);
                }
                return;
            }
            Hud::Output { ref name, worn } => {
                self.notice(
                    if worn {
                        GLYPH_HEADPHONES
                    } else {
                        GLYPH_SPEAKER
                    },
                    name,
                    TEXT,
                );
                return;
            }
            Hud::Language(ref name) => {
                self.notice(GLYPH_LANGUAGE, name, TEXT);
                return;
            }
        };
        self.text(glyph, &fonts.icon_large, 14.0, mid - 14.0, 32.0, 28.0, TEXT);
        let bar_x = 58.0;
        let bar_w = self.width - bar_x - 62.0;
        self.pill(bar_x, mid - 3.0, bar_w, 6.0, TRACK);
        self.pill(bar_x, mid - 3.0, bar_w * value.clamp(0.0, 1.0), 6.0, TEXT);
        let percent = format!("{}", (value * 100.0).round() as u32);
        self.text(
            &percent,
            &fonts.title,
            self.width - 54.0,
            mid - 11.0,
            40.0,
            22.0,
            TEXT,
        );
    }

    /// An icon and one line of text.
    fn notice(&self, glyph: &str, label: &str, color: D2D1_COLOR_F) {
        let fonts = &self.gfx.fonts;
        let mid = self.height / 2.0;
        self.text(glyph, &fonts.icon_large, 14.0, mid - 14.0, 32.0, 28.0, TEXT);
        // The label sits next to the icon in any script; right-to-left text would
        // otherwise start at the far end of the box.
        let rtl = is_rtl(label);
        unsafe {
            if rtl {
                let _ = fonts.title.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_TRAILING);
            }
        }
        self.text(
            label,
            &fonts.title,
            56.0,
            mid - 11.0,
            self.width - 72.0,
            22.0,
            color,
        );
        unsafe {
            if rtl {
                let _ = fonts.title.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_LEADING);
            }
        }
    }
}

/// True when the first letter of `text` is from a right-to-left script.
fn is_rtl(text: &str) -> bool {
    text.chars()
        .find(|c| c.is_alphabetic())
        .is_some_and(|c| matches!(
            c as u32,
            0x0590..=0x08FF | 0xFB1D..=0xFDFF | 0xFE70..=0xFEFF | 0x10800..=0x10FFF | 0x1E800..=0x1EFFF
        ))
}

fn point(x: f32, y: f32) -> Vector2 {
    Vector2 { X: x, Y: y }
}

fn rect(x: f32, y: f32, w: f32, h: f32) -> D2D_RECT_F {
    D2D_RECT_F {
        left: x,
        top: y,
        right: x + w,
        bottom: y + h,
    }
}

#[cfg(test)]
mod tests {
    use super::is_rtl;

    #[test]
    fn is_rtl_should_follow_the_first_letter() {
        assert_eq!(
            (
                is_rtl("2 | أحلام - حزين"),
                is_rtl("Ahlam | أحلام"),
                is_rtl("123")
            ),
            (true, false, false)
        );
    }
}

#[cfg(test)]
mod edge_case_tests {
    use super::is_rtl;

    #[test]
    fn right_to_left_scripts_beyond_the_basic_plane_should_be_recognised() {
        // Adlam (Fula) and Hanifi Rohingya.
        assert!(is_rtl("\u{1E900}\u{1E901}") && is_rtl("\u{10D00}"));
    }
}

#[cfg(test)]
mod equalizer_tests {
    use super::*;

    #[test]
    fn equalizer_bars_should_stay_in_range_and_move() {
        let frames: Vec<[f32; 3]> = (0..600)
            .map(|i| equalizer_heights(i as f32 / 30.0))
            .collect();
        let in_range = frames
            .iter()
            .flatten()
            .all(|h| (EQUALIZER_MIN..=EQUALIZER_MAX).contains(h));
        assert!(in_range && frames[0] != frames[3]);
    }
}
