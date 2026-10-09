//! Colors and themes: how cell colors from the terminal core become pixels.

use auk_term::{Color, Rgb};

/// A straight-alpha sRGB color.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Rgba(pub u8, pub u8, pub u8, pub u8);

impl Rgba {
    pub const TRANSPARENT: Rgba = Rgba(0, 0, 0, 0);

    pub const fn rgb(r: u8, g: u8, b: u8) -> Rgba {
        Rgba(r, g, b, 255)
    }

    pub const fn from_hex(hex: u32) -> Rgba {
        Rgba((hex >> 16) as u8, (hex >> 8) as u8, hex as u8, 255)
    }

    pub fn to_array(self) -> [u8; 4] {
        [self.0, self.1, self.2, self.3]
    }

    /// `self` mixed toward `other` by `t` (0 = self, 1 = other), alpha kept from `self`.
    pub fn mix(self, other: Rgba, t: f32) -> Rgba {
        let m = |a: u8, b: u8| (f32::from(a) + (f32::from(b) - f32::from(a)) * t).round() as u8;
        Rgba(
            m(self.0, other.0),
            m(self.1, other.1),
            m(self.2, other.2),
            self.3,
        )
    }
}

impl From<Rgb> for Rgba {
    fn from(c: Rgb) -> Self {
        Rgba(c.0, c.1, c.2, 255)
    }
}

/// The colors a grid draws with. Programs can override palette entries and the default colors
/// (OSC 4/10/11/12); [`Theme::with_overrides`] applies those.
#[derive(Clone, Debug, PartialEq)]
pub struct Theme {
    pub foreground: Rgba,
    pub background: Rgba,
    pub cursor: Rgba,
    /// Text under a block cursor.
    pub cursor_text: Rgba,
    pub selection_background: Rgba,
    /// Text in a selection; `None` keeps each cell's own color.
    pub selection_foreground: Option<Rgba>,
    /// The 256-color palette; 0–15 are the ANSI colors.
    pub palette: [Rgba; 256],
    /// Bold text in colors 0–7 draws in 8–15.
    pub bold_is_bright: bool,
    /// The accent a hovered link's underline draws in.
    pub link: Rgba,
}

/// The palette index OSC 10 (default foreground) overrides use in
/// `auk_term::Terminal::color_overrides`, following xterm's numbering after the 256 colors.
pub const OVERRIDE_FOREGROUND: u16 = 256;
/// The index of OSC 11 (default background) overrides.
pub const OVERRIDE_BACKGROUND: u16 = 257;
/// The index of OSC 12 (cursor color) overrides.
pub const OVERRIDE_CURSOR: u16 = 258;

impl Theme {
    /// A dark theme. Its default and ANSI colors are [`auk_term::Facts::dark`], which the
    /// daemon also answers color queries with.
    pub fn dark() -> Theme {
        let base = auk_term::Facts::dark();
        Theme {
            foreground: base.foreground.into(),
            background: base.background.into(),
            cursor: base.cursor.into(),
            cursor_text: base.background.into(),
            selection_background: Rgba::from_hex(0x37474f),
            selection_foreground: None,
            palette: xterm_palette(base.palette.map(Rgba::from)),
            bold_is_bright: false,
            link: Rgba::from_hex(0x6f9ef2),
        }
    }

    /// A light theme, from [`auk_term::Facts::light`] as [`Theme::dark`].
    pub fn light() -> Theme {
        let base = auk_term::Facts::light();
        Theme {
            foreground: base.foreground.into(),
            background: base.background.into(),
            cursor: base.cursor.into(),
            cursor_text: Rgba::from_hex(0xffffff),
            selection_background: Rgba::from_hex(0xb4d5fe),
            selection_foreground: None,
            palette: xterm_palette(base.palette.map(Rgba::from)),
            bold_is_bright: false,
            link: Rgba::from_hex(0x2563d9),
        }
    }

    /// This theme with a program's OSC 4/10/11/12 overrides applied.
    pub fn with_overrides(&self, overrides: &[(u16, Rgb)]) -> Theme {
        let mut theme = self.clone();
        for &(index, color) in overrides {
            match index {
                0..=255 => theme.palette[usize::from(index)] = color.into(),
                OVERRIDE_FOREGROUND => theme.foreground = color.into(),
                OVERRIDE_BACKGROUND => theme.background = color.into(),
                OVERRIDE_CURSOR => theme.cursor = color.into(),
                _ => {}
            }
        }
        theme
    }

    /// A cell color as drawn, for the foreground (`fg`) or background role.
    pub fn resolve(&self, color: Color, fg: bool, bold: bool) -> Rgba {
        match color {
            Color::Default if fg => self.foreground,
            Color::Default => self.background,
            Color::Indexed(i) if bold && fg && self.bold_is_bright && i < 8 => {
                self.palette[usize::from(i) + 8]
            }
            Color::Indexed(i) => self.palette[usize::from(i)],
            Color::Rgb(c) => c.into(),
        }
    }

    /// Client facts for the terminal core's color reports (OSC 4/10/11/12 queries).
    pub fn facts(&self, cell_px: (u16, u16)) -> auk_term::Facts {
        let rgb = |c: Rgba| Rgb(c.0, c.1, c.2);
        auk_term::Facts {
            cell_px,
            foreground: rgb(self.foreground),
            background: rgb(self.background),
            cursor: rgb(self.cursor),
            palette: std::array::from_fn(|i| rgb(self.palette[i])),
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Theme::dark()
    }
}

/// xterm's 256-color palette: the 16 given colors, the 6×6×6 cube and the gray ramp.
pub fn xterm_palette(ansi: [Rgba; 16]) -> [Rgba; 256] {
    std::array::from_fn(|i| match i {
        0..16 => ansi[i],
        16..232 => {
            let n = i - 16;
            let level = |v: usize| if v == 0 { 0 } else { (55 + v * 40) as u8 };
            Rgba::rgb(level(n / 36), level(n / 6 % 6), level(n % 6))
        }
        _ => {
            let v = (8 + (i - 232) * 10) as u8;
            Rgba::rgb(v, v, v)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cube_and_ramp_match_xterm() {
        let p = Theme::dark().palette;
        // Values from xterm's 256colres.h.
        assert_eq!(p[16], Rgba::rgb(0, 0, 0));
        assert_eq!(p[21], Rgba::rgb(0, 0, 0xff));
        assert_eq!(p[196], Rgba::rgb(0xff, 0, 0));
        assert_eq!(p[110], Rgba::rgb(0x87, 0xaf, 0xd7));
        assert_eq!(p[232], Rgba::rgb(8, 8, 8));
        assert_eq!(p[255], Rgba::rgb(0xee, 0xee, 0xee));
    }

    #[test]
    fn resolution_and_overrides() {
        let mut t = Theme::dark();
        assert_eq!(t.resolve(Color::Default, true, false), t.foreground);
        assert_eq!(t.resolve(Color::Default, false, false), t.background);
        assert_eq!(t.resolve(Color::Indexed(1), true, true), t.palette[1]);
        t.bold_is_bright = true;
        assert_eq!(t.resolve(Color::Indexed(1), true, true), t.palette[9]);
        assert_eq!(t.resolve(Color::Indexed(1), false, true), t.palette[1]);
        assert_eq!(
            t.resolve(Color::Rgb(Rgb(1, 2, 3)), true, false),
            Rgba::rgb(1, 2, 3)
        );
        let o = t.with_overrides(&[(4, Rgb(9, 9, 9)), (OVERRIDE_BACKGROUND, Rgb(1, 1, 1))]);
        assert_eq!(o.palette[4], Rgba::rgb(9, 9, 9));
        assert_eq!(o.background, Rgba::rgb(1, 1, 1));
    }
}
