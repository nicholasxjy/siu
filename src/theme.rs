//! Colors: one palette for dark terminals and one for light, picked from
//! the terminal's own background, every text color readable on either.
//!
//! The palettes start from Catppuccin Mocha and Latte, with the light one
//! darkened where its colors were too faint on white. The terminal's own
//! background and foreground are kept: siu paints neither.

use ratatui::style::Color;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Dark,
    Light,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    pub mode: Mode,
    /// Secondary text: paths, hints, counts.
    pub muted: Color,
    /// Lines only, never text: unfocused borders.
    pub faint: Color,
    /// What has focus, and keys to press.
    pub accent: Color,
    pub ok: Color,
    pub warn: Color,
    pub err: Color,
    /// Picked and official things.
    pub star: Color,
    /// The selected row's background.
    pub sel_bg: Color,
    /// Text on an accent background.
    pub on_accent: Color,
}

const fn hex(v: u32) -> Color {
    Color::Rgb((v >> 16) as u8, (v >> 8) as u8, v as u8)
}

impl Theme {
    pub const DARK: Theme = Theme {
        mode: Mode::Dark,
        muted: hex(0xa6adc8),
        faint: hex(0x6c7086),
        accent: hex(0x89b4fa),
        ok: hex(0xa6e3a1),
        warn: hex(0xf9e2af),
        err: hex(0xf38ba8),
        star: hex(0xcba6f7),
        sel_bg: hex(0x313244),
        on_accent: hex(0x11111b),
    };

    pub const LIGHT: Theme = Theme {
        mode: Mode::Light,
        muted: hex(0x565a70),
        faint: hex(0x8c8fa1),
        accent: hex(0x1a56db),
        ok: hex(0x1f6f2a),
        warn: hex(0x875400),
        err: hex(0xbf1130),
        star: hex(0x7232d6),
        sel_bg: hex(0xe3e7f2),
        on_accent: hex(0xffffff),
    };

    pub fn of(mode: Mode) -> Theme {
        match mode {
            Mode::Dark => Theme::DARK,
            Mode::Light => Theme::LIGHT,
        }
    }

    /// The theme for this terminal: its mode, in colors it can show.
    pub fn for_terminal(mode: Mode) -> Theme {
        Theme::of(mode).adapt(truecolor())
    }

    /// The same colors as the nearest of the 256 xterm ones, for a
    /// terminal that can't show any color.
    pub fn adapt(self, truecolor: bool) -> Theme {
        if truecolor {
            return self;
        }
        let f = |c: Color| match c {
            Color::Rgb(r, g, b) => Color::Indexed(ansi256(r, g, b)),
            c => c,
        };
        Theme {
            mode: self.mode,
            muted: f(self.muted),
            faint: f(self.faint),
            accent: f(self.accent),
            ok: f(self.ok),
            warn: f(self.warn),
            err: f(self.err),
            star: f(self.star),
            sel_bg: f(self.sel_bg),
            on_accent: f(self.on_accent),
        }
    }

    pub fn toggled(self) -> Theme {
        let other = match self.mode {
            Mode::Dark => Mode::Light,
            Mode::Light => Mode::Dark,
        };
        let truecolor = matches!(self.accent, Color::Rgb(..));
        Theme::of(other).adapt(truecolor)
    }
}

pub fn parse_mode(s: &str) -> Option<Mode> {
    match s.trim().to_ascii_lowercase().as_str() {
        "dark" => Some(Mode::Dark),
        "light" => Some(Mode::Light),
        _ => None,
    }
}

/// The mode `COLORFGBG` ("fg;bg", set by rxvt, Konsole and others) says.
pub fn mode_from_colorfgbg(v: &str) -> Option<Mode> {
    let bg: u8 = v.rsplit(';').next()?.parse().ok()?;
    // 0-6 and 8 are the dark ones of the 16 colors; 7 and 9-15 are light
    Some(if bg == 7 || bg >= 9 {
        Mode::Light
    } else {
        Mode::Dark
    })
}

/// The terminal's mode: `SIU_THEME` when it says dark or light, else
/// what the terminal answers about its background, else `COLORFGBG`,
/// else dark. Asks the terminal, so it runs before the TUI takes it over.
pub fn detect() -> Mode {
    if let Some(m) = std::env::var("SIU_THEME")
        .ok()
        .as_deref()
        .and_then(parse_mode)
    {
        return m;
    }
    let mut opts = terminal_colorsaurus::QueryOptions::default();
    opts.timeout = std::time::Duration::from_millis(300);
    match terminal_colorsaurus::theme_mode(opts) {
        Ok(terminal_colorsaurus::ThemeMode::Light) => return Mode::Light,
        Ok(terminal_colorsaurus::ThemeMode::Dark) => return Mode::Dark,
        Err(_) => {}
    }
    std::env::var("COLORFGBG")
        .ok()
        .as_deref()
        .and_then(mode_from_colorfgbg)
        .unwrap_or(Mode::Dark)
}

/// Whether the terminal shows 24-bit color.
pub fn truecolor() -> bool {
    let var = |k: &str| std::env::var(k).unwrap_or_default().to_ascii_lowercase();
    if matches!(var("COLORTERM").as_str(), "truecolor" | "24bit") {
        return true;
    }
    if var("TERM_PROGRAM") == "apple_terminal" {
        return false;
    }
    let term = var("TERM");
    ["iterm", "wezterm", "ghostty", "vscode"]
        .iter()
        .any(|t| var("TERM_PROGRAM").contains(t))
        || ["kitty", "alacritty", "ghostty", "foot", "direct"]
            .iter()
            .any(|t| term.contains(t))
}

/// The nearest of xterm's 256 colors: the 6×6×6 cube or the gray ramp.
pub fn ansi256(r: u8, g: u8, b: u8) -> u8 {
    const STEPS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    let near = |v: u8| {
        STEPS
            .iter()
            .enumerate()
            .min_by_key(|(_, s)| (**s as i32 - v as i32).abs())
            .unwrap()
            .0
    };
    let (ri, gi, bi) = (near(r), near(g), near(b));
    let cube = 16 + 36 * ri + 6 * gi + bi;
    let cube_rgb = (STEPS[ri], STEPS[gi], STEPS[bi]);
    let avg = (r as u32 + g as u32 + b as u32) / 3;
    let gi = ((avg.saturating_sub(8)) / 10).min(23);
    let gray = 8 + 10 * gi;
    let dist = |(x, y, z): (u8, u8, u8)| {
        let d = |a: u8, b: u8| (a as i32 - b as i32).pow(2);
        d(x, r) + d(y, g) + d(z, b)
    };
    if dist((gray as u8, gray as u8, gray as u8)) < dist(cube_rgb) {
        232 + gi as u8
    } else {
        cube as u8
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgb(c: Color) -> (u8, u8, u8) {
        match c {
            Color::Rgb(r, g, b) => (r, g, b),
            _ => panic!("{c:?}"),
        }
    }

    fn lum((r, g, b): (u8, u8, u8)) -> f64 {
        let ch = |v: u8| {
            let v = v as f64 / 255.0;
            if v <= 0.03928 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * ch(r) + 0.7152 * ch(g) + 0.0722 * ch(b)
    }

    /// WCAG's contrast ratio.
    fn contrast(a: (u8, u8, u8), b: (u8, u8, u8)) -> f64 {
        let (x, y) = (lum(a), lum(b));
        (x.max(y) + 0.05) / (x.min(y) + 0.05)
    }

    fn h(v: u32) -> (u8, u8, u8) {
        rgb(hex(v))
    }

    /// Backgrounds terminals commonly ship with.
    const DARK_BGS: [u32; 5] = [0x000000, 0x1e1e2e, 0x282c34, 0x002b36, 0x2e3440];
    const LIGHT_BGS: [u32; 5] = [0xffffff, 0xeff1f5, 0xfdf6e3, 0xf5f5f5, 0xe5e9f0];

    #[test]
    fn every_text_color_is_readable_on_common_backgrounds() {
        for (t, bgs) in [(Theme::DARK, DARK_BGS), (Theme::LIGHT, LIGHT_BGS)] {
            let text = [
                ("muted", t.muted),
                ("accent", t.accent),
                ("ok", t.ok),
                ("warn", t.warn),
                ("err", t.err),
                ("star", t.star),
            ];
            for (name, c) in text {
                for bg in bgs {
                    let r = contrast(rgb(c), h(bg));
                    assert!(r >= 4.5, "{:?} {name} on #{bg:06x}: {r:.2}", t.mode);
                }
                // and on the selected row
                let r = contrast(rgb(c), rgb(t.sel_bg));
                assert!(r >= 4.5, "{:?} {name} on the selection: {r:.2}", t.mode);
            }
            for bg in bgs {
                assert!(
                    contrast(rgb(t.faint), h(bg)) >= 2.2,
                    "{:?} borders on #{bg:06x}",
                    t.mode
                );
            }
            assert!(
                contrast(rgb(t.on_accent), rgb(t.accent)) >= 4.5,
                "{:?} badge",
                t.mode
            );
        }
    }

    #[test]
    fn modes_from_the_environment() {
        assert_eq!(parse_mode(" Light "), Some(Mode::Light));
        assert_eq!(parse_mode("auto"), None);
        assert_eq!(mode_from_colorfgbg("15;0"), Some(Mode::Dark));
        assert_eq!(mode_from_colorfgbg("0;15"), Some(Mode::Light));
        assert_eq!(mode_from_colorfgbg("0;default;7"), Some(Mode::Light));
        assert_eq!(mode_from_colorfgbg("default;default"), None);
    }

    #[test]
    fn colors_fall_back_to_the_256_palette() {
        assert_eq!(ansi256(0, 0, 0), 16);
        assert_eq!(ansi256(255, 255, 255), 231);
        assert_eq!(ansi256(128, 128, 128), 244);
        assert_eq!(ansi256(0x89, 0xb4, 0xfa), 111);
        let t = Theme::LIGHT.adapt(false);
        assert!(matches!(t.accent, Color::Indexed(_)));
        assert_eq!(Theme::LIGHT.adapt(true), Theme::LIGHT);
        assert_eq!(t.toggled().mode, Mode::Dark);
        assert!(matches!(t.toggled().accent, Color::Indexed(_)));
        assert_eq!(Theme::DARK.toggled(), Theme::LIGHT);
    }
}
