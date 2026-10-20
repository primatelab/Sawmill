//! Built-in themes. `terminal` is Sawmill's original ANSI look. The rest are
//! Herdr's truecolor palettes, plus Monokai. A dark theme's background is slot
//! 7, a light theme's is slot 1.

use crate::parse::Severity;
use ratatui::style::Color;
use std::sync::OnceLock;

pub const NAMES: &[&str] = &[
    "catppuccin",
    "catppuccin-latte",
    "terminal",
    "tokyo-night",
    "tokyo-night-day",
    "dracula",
    "nord",
    "gruvbox",
    "gruvbox-light",
    "one-dark",
    "one-light",
    "solarized",
    "solarized-light",
    "kanagawa",
    "kanagawa-lotus",
    "rose-pine",
    "rose-pine-dawn",
    "vesper",
    "monokai",
];

#[derive(Clone, Copy)]
pub struct Theme {
    pub name: &'static str,
    pub light: bool,
    pub bg: Color,
    pub text: Color,
    pub bright: Color,
    pub dim: Color,
    /// Status badges, menu highlight, help keys, gauge fill.
    pub accent: Color,
    pub cursor: Color,
    /// Lit histogram column.
    pub shade: Color,
    /// Gauge track.
    pub gauge: Color,
    /// A level switched off in the status bar.
    pub inactive: Color,
    pub error: Color,
    pub error_lit: Color,
    pub warn: Color,
    pub warn_lit: Color,
    pub info: Color,
    pub info_lit: Color,
    pub debug: Color,
    pub debug_lit: Color,
    pub other: Color,
    pub other_lit: Color,
    pub green: Color,
    pub yellow: Color,
    pub red: Color,
    pub blue: Color,
    pub cyan: Color,
    pub purple: Color,
    pub find_bg: Color,
    pub select_bg: Color,
}

const fn rgb(hex: u32) -> Color {
    Color::Rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

/// The colours the TUI used before themes. Histogram tests compare these.
pub const TERMINAL: Theme = Theme {
    name: "terminal",
    light: false,
    bg: Color::Reset,
    text: Color::Gray,
    bright: Color::White,
    dim: Color::DarkGray,
    accent: Color::Cyan,
    cursor: Color::Indexed(238),
    shade: Color::Indexed(237),
    gauge: Color::Indexed(236),
    inactive: Color::Indexed(238),
    error: Color::Red,
    error_lit: Color::LightRed,
    warn: Color::Yellow,
    warn_lit: Color::LightYellow,
    info: Color::Cyan,
    info_lit: Color::LightCyan,
    debug: Color::Blue,
    debug_lit: Color::LightBlue,
    other: Color::DarkGray,
    other_lit: Color::Gray,
    green: Color::Green,
    yellow: Color::Yellow,
    red: Color::Red,
    blue: Color::Blue,
    cyan: Color::Cyan,
    purple: Color::Magenta,
    find_bg: Color::LightGreen,
    select_bg: Color::LightBlue,
};

struct Pal {
    name: &'static str,
    light: bool,
    c: [u32; 19],
}

fn from_palette(p: &Pal) -> Theme {
    let c = |i: usize| rgb(p.c[i]);
    let bg = if p.light { c(1) } else { c(7) };
    let toward = if p.light { rgb(0) } else { rgb(0xffffff) };
    let lit = |color: Color| blend(color, toward, 100);
    let shade = if c(6) == bg { c(5) } else { c(6) };
    let cursor = if c(4) == bg { shade } else { c(4) };
    let gauge = if c(5) == bg { shade } else { c(5) };
    Theme {
        name: p.name,
        light: p.light,
        bg,
        text: c(10),
        bright: c(10),
        dim: c(8),
        accent: c(0),
        cursor,
        shade,
        gauge,
        inactive: c(8),
        error: c(15),
        error_lit: lit(c(15)),
        warn: c(14),
        warn_lit: lit(c(14)),
        info: c(17),
        info_lit: lit(c(17)),
        debug: c(16),
        debug_lit: lit(c(16)),
        other: c(8),
        other_lit: lit(c(8)),
        green: c(13),
        yellow: c(14),
        red: c(15),
        blue: c(16),
        cyan: c(17),
        purple: c(12),
        find_bg: c(13),
        select_bg: c(16),
    }
}

/// `name` is matched exactly. Anything else is the terminal theme.
pub fn by_name(name: &str) -> &'static Theme {
    all().iter().find(|t| t.name == name).unwrap_or(&TERMINAL)
}

fn all() -> &'static [Theme] {
    static ALL: OnceLock<Vec<Theme>> = OnceLock::new();
    ALL.get_or_init(|| {
        let mut palettes = PALETTES.iter();
        NAMES
            .iter()
            .map(|name| {
                if *name == "terminal" {
                    TERMINAL
                } else {
                    from_palette(palettes.next().expect("a palette per theme"))
                }
            })
            .collect()
    })
}

impl Theme {
    pub fn severity(self, s: Severity) -> Color {
        match s {
            Severity::Error => self.error,
            Severity::Warn => self.warn,
            Severity::Info => self.info,
            Severity::Debug => self.debug,
            Severity::Other => self.other,
        }
    }

    pub fn severity_lit(self, s: Severity) -> Color {
        match s {
            Severity::Error => self.error_lit,
            Severity::Warn => self.warn_lit,
            Severity::Info => self.info_lit,
            Severity::Debug => self.debug_lit,
            Severity::Other => self.other_lit,
        }
    }

    /// A colour name follows the theme. `#rrggbb` and a 256-colour index do not.
    /// The terminal theme leaves every name as its ANSI colour.
    pub fn resolve(self, spec: &str) -> Result<Color, String> {
        if self.name != "terminal"
            && let Some(c) = self.named(&canon(spec))
        {
            return Ok(c);
        }
        spec.parse().map_err(|_| format!("unknown colour {spec:?}"))
    }

    fn named(self, spec: &str) -> Option<Color> {
        let lit = |c: Color| blend(c, if self.light { rgb(0) } else { rgb(0xffffff) }, 100);
        Some(match spec {
            "black" => {
                if self.bg == Color::Reset {
                    Color::Black
                } else {
                    self.bg
                }
            }
            "red" => self.red,
            "lightred" => self.error_lit,
            "green" => self.green,
            "lightgreen" => lit(self.green),
            "yellow" => self.yellow,
            "lightyellow" => self.warn_lit,
            "blue" => self.blue,
            "lightblue" => self.debug_lit,
            "magenta" => self.purple,
            "lightmagenta" => lit(self.purple),
            "cyan" => self.cyan,
            "lightcyan" => self.info_lit,
            "white" | "gray" => self.bright,
            "darkgray" => self.dim,
            _ => return None,
        })
    }
}

/// Text colour that stays readable on `bg`.
pub fn ink(bg: Color) -> Color {
    match bg {
        Color::Rgb(r, g, b) => {
            let luma = r as u16 * 3 + g as u16 * 6 + b as u16;
            if luma > 255 * 5 { Color::Black } else { Color::White }
        }
        Color::White | Color::Gray | Color::Yellow | Color::Green | Color::Cyan | Color::LightRed | Color::LightGreen
        | Color::LightYellow | Color::LightBlue | Color::LightMagenta | Color::LightCyan => Color::Black,
        _ => Color::White,
    }
}

fn blend(c: Color, toward: Color, t: u16) -> Color {
    let (Color::Rgb(r, g, b), Color::Rgb(r2, g2, b2)) = (c, toward) else { return c };
    let mix = |a: u8, b: u8| ((u16::from(a) * (255 - t) + u16::from(b) * t) / 255) as u8;
    Color::Rgb(mix(r, r2), mix(g, g2), mix(b, b2))
}

/// Same folding as ratatui's colour parser, so `light-red` and `grey` map too.
fn canon(spec: &str) -> String {
    spec.to_lowercase()
        .replace([' ', '-', '_'], "")
        .replace("bright", "light")
        .replace("grey", "gray")
        .replace("silver", "gray")
        .replace("lightblack", "darkgray")
        .replace("lightwhite", "white")
        .replace("lightgray", "white")
}

/// Slot 2 is unused. Dark themes take their background from slot 7, light ones from slot 1.
const PALETTES: &[Pal] = &[
    Pal {
        name: "catppuccin",
        light: false,
        c: [0x89b4fa, 0x181825, 0, 0x1e1e2e, 0x313244, 0x313244, 0x45475a, 0x1e1e2e, 0x6c7086, 0x7f849c, 0xcdd6f4, 0xa6adc8, 0xcba6f7, 0xa6e3a1, 0xf9e2af, 0xf38ba8, 0x89b4fa, 0x94e2d5, 0xfab387],
    },
    Pal {
        name: "catppuccin-latte",
        light: true,
        c: [0x1e66f5, 0xeff1f5, 0, 0xe6e9ef, 0xbdd0f5, 0xccd0da, 0xbcc0cc, 0xe6e9ef, 0x9ca0b0, 0x8c8fa1, 0x4c4f69, 0x6c6f85, 0x8839ef, 0x40a02b, 0xdf8e1d, 0xd20f39, 0x1e66f5, 0x179299, 0xfe640b],
    },
    Pal {
        name: "tokyo-night",
        light: false,
        c: [0x7aa2f7, 0x1a1b26, 0, 0x232636, 0x2d3650, 0x24283b, 0x414868, 0x1a1b26, 0x565f89, 0x697196, 0xc0caf5, 0xa9b1d6, 0xbb9af7, 0x9ece6a, 0xe0af68, 0xf7768e, 0x7aa2f7, 0x7dcfff, 0xff9e64],
    },
    Pal {
        name: "tokyo-night-day",
        light: true,
        c: [0x2e7de9, 0xe1e2e7, 0, 0xd2d3da, 0xb6cae7, 0xc4c8da, 0xa8aecb, 0xd2d3da, 0x8990b3, 0x68709a, 0x3760bf, 0x6172b0, 0x7847bd, 0x587539, 0x8c6c3e, 0xf52a65, 0x2e7de9, 0x118c74, 0xb15c00],
    },
    Pal {
        name: "dracula",
        light: false,
        c: [0xbd93f9, 0x282a36, 0, 0x373c52, 0x463f5d, 0x44475a, 0x6272a4, 0x282a36, 0x6272a4, 0x828cb4, 0xf8f8f2, 0xd2d2dc, 0xff79c6, 0x50fa7b, 0xf1fa8c, 0xff5555, 0x8be9fd, 0x8be9fd, 0xffb86c],
    },
    Pal {
        name: "nord",
        light: false,
        c: [0x88c0d0, 0x2e3440, 0, 0x434c5e, 0x40505d, 0x3b4252, 0x434c5e, 0x2e3440, 0x4c566a, 0x646e82, 0xeceff4, 0xd8dee9, 0xb48ead, 0xa3be8c, 0xebcb8b, 0xbf616a, 0x81a1c1, 0x8fbcbb, 0xd08770],
    },
    Pal {
        name: "gruvbox",
        light: false,
        c: [0xd79921, 0x282828, 0, 0x323130, 0x4b3f27, 0x3c3836, 0x504945, 0x282828, 0x928374, 0xa89984, 0xebdbb2, 0xd5c4a1, 0xd3869b, 0xb8bb26, 0xfabd2f, 0xfb4934, 0x83a598, 0x8ec07c, 0xfe8019],
    },
    Pal {
        name: "gruvbox-light",
        light: true,
        c: [0x076678, 0xfbf1c7, 0, 0xf2e5bc, 0xebdbb2, 0xebdbb2, 0xd5c4a1, 0xf2e5bc, 0x928374, 0x7c6f64, 0x3c3836, 0x504945, 0x8f3f71, 0x79740e, 0xb57614, 0x9d0006, 0x076678, 0x427b58, 0xaf3a03],
    },
    Pal {
        name: "one-dark",
        light: false,
        c: [0x61afef, 0x282c34, 0, 0x313640, 0x334659, 0x2c313a, 0x3e4451, 0x282c34, 0x5c6370, 0x737a87, 0xabb2bf, 0x969ca8, 0xc678dd, 0x98c379, 0xe5c07b, 0xe06c75, 0x61afef, 0x56b6c2, 0xd19a66],
    },
    Pal {
        name: "one-light",
        light: true,
        c: [0x4078f2, 0xfafafa, 0, 0xd8dbe2, 0xcddbf8, 0xf0f0f1, 0xe5e5e6, 0xf5f5f6, 0xa0a1a7, 0x686b77, 0x383a42, 0x686b77, 0xa626a4, 0x50a14f, 0xc18401, 0xe45649, 0x4078f2, 0x0184bc, 0x986801],
    },
    Pal {
        name: "solarized",
        light: false,
        c: [0x268bd2, 0x002b36, 0, 0x164b57, 0x083e55, 0x073642, 0x586e75, 0x002b36, 0x586e75, 0x657b83, 0x93a1a1, 0x839496, 0xd33682, 0x859900, 0xb58900, 0xdc322f, 0x268bd2, 0x2aa198, 0xcb4b16],
    },
    Pal {
        name: "solarized-light",
        light: true,
        c: [0x268bd2, 0xfdf6e3, 0, 0xeee8d5, 0xc9dcdf, 0xeee8d5, 0x93a1a1, 0xeee8d5, 0x93a1a1, 0x586e75, 0x657b83, 0x839496, 0xd33682, 0x859900, 0xb58900, 0xdc322f, 0x268bd2, 0x2aa198, 0xcb4b16],
    },
    Pal {
        name: "kanagawa",
        light: false,
        c: [0x7e9cd8, 0x1f1f28, 0, 0x363646, 0x32384b, 0x2a2a37, 0x363646, 0x1f1f28, 0x727169, 0x87867d, 0xdcd7ba, 0xc8c3aa, 0x957fb8, 0x76946a, 0xc0a36e, 0xc34043, 0x7e9cd8, 0x7fb4ca, 0xffa066],
    },
    Pal {
        name: "kanagawa-lotus",
        light: true,
        c: [0x4d699b, 0xf2ecbc, 0, 0xd5cea3, 0xdcd5ac, 0xdcd5ac, 0xc9cbd1, 0xd5cea3, 0xa09cac, 0x8a8980, 0x545464, 0x43436c, 0x624c83, 0x6f894e, 0x77713f, 0xc84053, 0x4d699b, 0x4e8ca2, 0xcc6d00],
    },
    Pal {
        name: "rose-pine",
        light: false,
        c: [0xc4a7e7, 0x191724, 0, 0x26233a, 0x3b344b, 0x1f1d2e, 0x26233a, 0x26233a, 0x6e6a86, 0x908caa, 0xe0def4, 0xc8c5dc, 0xc4a7e7, 0x31748f, 0xf6c177, 0xeb6f92, 0x31748f, 0x9ccfd8, 0xea9a97],
    },
    Pal {
        name: "rose-pine-dawn",
        light: true,
        c: [0x907aa9, 0xfaf4ed, 0, 0xe3d9cf, 0xf2e9e1, 0xf2e9e1, 0xfffaf3, 0xf2e9e1, 0x9893a5, 0x797593, 0x464261, 0x797593, 0x907aa9, 0x286983, 0xea9d34, 0xb4637a, 0x286983, 0x56949f, 0xd7827e],
    },
    Pal {
        name: "vesper",
        light: false,
        c: [0xffc799, 0x1a1a1a, 0, 0x101010, 0x232323, 0x232323, 0x282828, 0x101010, 0x5c5c5c, 0x7e7e7e, 0xffffff, 0xa0a0a0, 0xffd1a8, 0x99ffe4, 0xffc799, 0xff8080, 0xb0b0b0, 0x66ddcc, 0xffc799],
    },
    // Original Monokai, as recorded by the VS Code theme: bg #272822, sidebar
    // #1e1f1c, line highlight #3e3d32, selection #49483e, comment #75715e,
    // fg #f8f8f2, pink #f92672, orange #fd971f, yellow #e6db74, green #a6e22e,
    // blue #66d9ef, purple #ae81ff. Blue and cyan are the same colour there.
    Pal {
        name: "monokai",
        light: false,
        c: [0xa6e22e, 0x1e1f1c, 0, 0x3e3d32, 0x49483e, 0x3e3d32, 0x414339, 0x272822, 0x75715e, 0x90908a, 0xf8f8f2, 0xc2c2bf, 0xae81ff, 0xa6e22e, 0xe6db74, 0xf92672, 0x66d9ef, 0x66d9ef, 0xfd971f],
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_listed_name_resolves_and_an_unknown_one_is_terminal() {
        assert_eq!(NAMES.len(), 19);
        for name in NAMES {
            assert_eq!(by_name(name).name, *name);
        }
        assert_eq!(by_name("no-such-theme").name, "terminal");
        assert_eq!(by_name("terminal").resolve("green").unwrap(), Color::Green);
    }

    #[test]
    fn a_named_colour_maps_and_a_hex_colour_does_not() {
        let dracula = by_name("dracula");
        assert_eq!(dracula.resolve("green").unwrap(), Color::Rgb(0x50, 0xfa, 0x7b));
        assert_eq!(dracula.resolve("light-red").unwrap(), dracula.error_lit);
        assert_eq!(dracula.resolve("#112233").unwrap(), Color::Rgb(0x11, 0x22, 0x33));
        assert_eq!(dracula.resolve("236").unwrap(), Color::Indexed(236));
        assert!(dracula.resolve("mauve").is_err());
        assert_eq!(by_name("solarized-light").bg, Color::Rgb(0xfd, 0xf6, 0xe3));
        assert_eq!(by_name("vesper").bg, Color::Rgb(0x10, 0x10, 0x10));
        let monokai = by_name("monokai");
        assert_eq!(monokai.bg, Color::Rgb(0x27, 0x28, 0x22));
        assert_eq!(monokai.resolve("green").unwrap(), Color::Rgb(0xa6, 0xe2, 0x2e));
        assert_eq!(monokai.resolve("red").unwrap(), Color::Rgb(0xf9, 0x26, 0x72));
    }
}
