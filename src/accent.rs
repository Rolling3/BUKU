//! The accent colour: switches, the music bars, the focus countdown and other
//! highlights. Chosen in Settings; read wherever something is drawn.

use std::sync::atomic::{AtomicU8, Ordering};

use serde::{Deserialize, Serialize};
use windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F;

use crate::gfx::rgba;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Accent {
    #[default]
    Yellow,
    Orange,
    Red,
    Pink,
    Purple,
    Blue,
    Cyan,
    Green,
    White,
}

impl Accent {
    pub const ALL: [Accent; 9] = [
        Accent::Yellow,
        Accent::Orange,
        Accent::Red,
        Accent::Pink,
        Accent::Purple,
        Accent::Blue,
        Accent::Cyan,
        Accent::Green,
        Accent::White,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Accent::Yellow => "Yellow",
            Accent::Orange => "Orange",
            Accent::Red => "Red",
            Accent::Pink => "Pink",
            Accent::Purple => "Purple",
            Accent::Blue => "Blue",
            Accent::Cyan => "Cyan",
            Accent::Green => "Green",
            Accent::White => "White",
        }
    }

    pub fn color(self) -> D2D1_COLOR_F {
        match self {
            Accent::Yellow => rgba(1.0, 0.70, 0.28, 1.0),
            Accent::Orange => rgba(1.0, 0.52, 0.20, 1.0),
            Accent::Red => rgba(0.96, 0.32, 0.32, 1.0),
            Accent::Pink => rgba(0.98, 0.42, 0.68, 1.0),
            Accent::Purple => rgba(0.68, 0.50, 0.98, 1.0),
            Accent::Blue => rgba(0.32, 0.58, 1.0, 1.0),
            Accent::Cyan => rgba(0.25, 0.80, 0.90, 1.0),
            Accent::Green => rgba(0.36, 0.82, 0.46, 1.0),
            Accent::White => rgba(0.94, 0.94, 0.94, 1.0),
        }
    }

    /// The next (`forward`) or previous colour, wrapping around.
    pub fn step(self, forward: bool) -> Self {
        let index = Self::ALL.iter().position(|&a| a == self).unwrap_or(0);
        let len = Self::ALL.len();
        Self::ALL[if forward {
            (index + 1) % len
        } else {
            (index + len - 1) % len
        }]
    }
}

/// The accent in use. The notch and the Settings window draw on the same
/// thread, so they always agree.
static CURRENT: AtomicU8 = AtomicU8::new(0);

pub fn set_current(accent: Accent) {
    let index = Accent::ALL.iter().position(|&a| a == accent).unwrap_or(0);
    CURRENT.store(index as u8, Ordering::Relaxed);
}

/// The colour of the accent in use.
pub fn current() -> D2D1_COLOR_F {
    Accent::ALL
        .get(CURRENT.load(Ordering::Relaxed) as usize)
        .copied()
        .unwrap_or_default()
        .color()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn step_should_visit_every_colour_and_wrap() {
        let mut accent = Accent::Yellow;
        let mut seen = Vec::new();
        for _ in 0..Accent::ALL.len() {
            seen.push(accent);
            accent = accent.step(true);
        }
        assert_eq!((seen, accent), (Accent::ALL.to_vec(), Accent::Yellow));
        assert_eq!(Accent::Yellow.step(false), Accent::White);
    }

    #[test]
    fn colours_should_be_distinct_and_yellow_the_old_one() {
        let colors: Vec<_> = Accent::ALL
            .iter()
            .map(|a| {
                let c = a.color();
                (c.r.to_bits(), c.g.to_bits(), c.b.to_bits())
            })
            .collect();
        let unique = colors
            .iter()
            .enumerate()
            .all(|(i, c)| !colors[..i].contains(c));
        let yellow = Accent::Yellow.color();
        assert!(unique);
        assert_eq!((yellow.r, yellow.g, yellow.b), (1.0, 0.70, 0.28));
    }

    #[test]
    fn a_name_from_the_settings_file_should_read_back() {
        let names = Accent::ALL.map(|a| serde_json::to_string(&a).unwrap_or_default());
        let back: Vec<Accent> = names
            .iter()
            .filter_map(|n| serde_json::from_str(n).ok())
            .collect();
        assert_eq!(back, Accent::ALL.to_vec());
    }
}
