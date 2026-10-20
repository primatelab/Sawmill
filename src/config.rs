//! `~/.config/sawmill/config.toml`: masks, macros, highlight rules and the UI
//! settings. A section left out of the file falls back to the built-in default,
//! which is `DEFAULT` below and what a first run writes out. Keys left out of
//! `[ui]` fall back one by one.

use crate::theme::Theme;
use ratatui::style::{Color, Modifier, Style};
use regex::Regex;
use serde::Deserialize;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::OnceLock;

pub const DEFAULT: &str = r##"# Sawmill — Dissecting Logs.
# Delete a section to get the built-in default for it back.

# theme is one of the built-in names (terminal, dracula, solarized, ...).
# wrap, mouse, case_sensitive and power_scroll are the Settings menu.
[ui]
theme = "terminal"
wrap = false
mouse = true
case_sensitive = false
power_scroll = false

# Masks collapse the variable parts of a line so that similar lines group
# into one pattern. They run in order, so put specific shapes before general
# ones. The name is shown uppercase and struck through in place of the match.
[[mask]]
name = "UUID"
regex = '\b[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}\b'

[[mask]]
name = "ADDR"
regex = '\d{1,3}\.\d{1,3}\.\d{1,3}\.\d{1,3}(:\d+)?\b'

[[mask]]
name = "TS"
regex = '\b\d{4}[-/]\d{2}[-/]\d{2}([T ]\d{2}:\d{2}:\d{2}(\.\d+)?)?\b'

[[mask]]
name = "TS"
regex = '\b\d{2}:\d{2}:\d{2}(\.\d+)?\b'

[[mask]]
name = "HEX"
regex = '\b0x[0-9a-fA-F]+\b'

[[mask]]
name = "HASH"
regex = '\b[0-9a-fA-F]{12,}\b'

# [[mask]]
# name = "PATH"
# regex = '/[A-Za-z0-9_.\-/]{2,}'

[[mask]]
name = "STR"
regex = '"[^"]*"'

[[mask]]
name = "STR"
regex = "'[^']*'"

# No trailing \b: numbers are often glued to a unit, as in "30s".
[[mask]]
name = "NUM"
regex = '\b\d+(\.\d+)?'

# Macros expand inside any regex you type: %IP%, %UUID%. %FIND% is built in
# and stands for the current find term.
[macros]
IP = '\b\d{1,3}(?:\.\d{1,3}){3}\b'
UUID = '\b[0-9A-Fa-f]{8}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{12}\b'

# Highlight rules colour raw lines and pattern text. A mask name is drawn
# struck through, over the top of these rules. Later rules paint over earlier ones.
# fg and bg take a colour name (red, lightred, darkgray, ...), "#rrggbb" or a
# 256-colour index. A name follows the current theme; a hex colour or an index
# stays as written. bold, italic, dim and underline are true or false.
[[highlight]] # Braces
regex = '[{}]'
fg = "green"

[[highlight]] # Python output
regex = '#012 *File|, line [0-9]+,'
fg = "green"

[[highlight]] # Module/Object
regex = '\b[A-Za-z0-9_-]+(::[A-Za-z0-9_-]+)+'
fg = "cyan"

[[highlight]] # Paths
regex = '~*/[A-Za-z_./]*/[A-Za-z0-9_./-]*'
fg = "yellow"
dim = true

[[highlight]] # Date
regex = '\b(?:Jan|Feb|Mar|Apr|May|Jun|Jul|Aug|Sep|Oct|Nov|Dec) +[0-9]{1,2}\b'
fg = "white"
italic = true

[[highlight]] # Date
regex = '\b20[0-9]{2}[/-][01][0-9][/-][0-3][0-9]\b'
fg = "white"
italic = true

[[highlight]] # Timestamp
regex = '\b[0-2][0-9]:[0-5][0-9]:[0-6][0-9](?:[.,][0-9]+)?\b'
fg = "white"
italic = true

[[highlight]] # IPv4 Address
regex = '[0-9]{1,3}(?:\.[0-9]{1,3}){3}\b'
fg = "lightred"
bold = true

[[highlight]] # UUID
regex = '\b[0-9A-Fa-f]{8}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{12}\b'
fg = "cyan"
"##;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    mask: Option<Vec<MaskSpec>>,
    macros: Option<toml::Table>,
    highlight: Option<Vec<HighlightSpec>>,
    ui: Option<UiSpec>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MaskSpec {
    name: String,
    regex: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HighlightSpec {
    regex: String,
    fg: Option<String>,
    bg: Option<String>,
    #[serde(default)]
    bold: bool,
    #[serde(default)]
    italic: bool,
    #[serde(default)]
    dim: bool,
    #[serde(default)]
    underline: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UiSpec {
    #[serde(default = "default_theme")]
    theme: String,
    #[serde(default)]
    wrap: bool,
    #[serde(default = "default_mouse")]
    mouse: bool,
    #[serde(default)]
    case_sensitive: bool,
    #[serde(default)]
    power_scroll: bool,
}

fn default_theme() -> String {
    "terminal".to_string()
}

fn default_mouse() -> bool {
    true
}

pub struct Mask {
    pub name: String,
    pub re: Regex,
}

#[derive(Clone)]
pub struct Ui {
    pub theme: String,
    pub wrap: bool,
    pub mouse: bool,
    pub case_sensitive: bool,
    pub power_scroll: bool,
}

impl Default for Ui {
    fn default() -> Self {
        Self {
            theme: default_theme(),
            wrap: false,
            mouse: true,
            case_sensitive: false,
            power_scroll: false,
        }
    }
}

pub struct Highlight {
    pub re: Regex,
    pub fg: Option<String>,
    pub bg: Option<String>,
    mods: Modifier,
}

impl Highlight {
    /// Colour names are resolved against `theme` at draw time.
    pub fn style(&self, theme: &Theme) -> Style {
        let mut style = Style::new();
        if let Some(fg) = &self.fg
            && let Ok(c) = theme.resolve(fg)
        {
            style = style.fg(c);
        }
        if let Some(bg) = &self.bg
            && let Ok(c) = theme.resolve(bg)
        {
            style = style.bg(c);
        }
        style.add_modifier(self.mods)
    }
}

pub struct Config {
    pub masks: Vec<Mask>,
    /// `(%NAME%, regex)`, matched case-insensitively.
    pub macros: Vec<(String, String)>,
    pub highlight: Vec<Highlight>,
    pub ui: Ui,
}

static CONFIG: OnceLock<Config> = OnceLock::new();

/// The loaded config, or the built-in one if `init` was never called.
pub fn get() -> &'static Config {
    CONFIG.get_or_init(|| parse(DEFAULT).expect("built-in config parses"))
}

/// Theme and Settings-menu options from the loaded config.
pub fn ui() -> Ui {
    get().ui.clone()
}

/// Read the config file, writing the default out first if there is none.
/// Call before anything parses a line.
pub fn init() -> Result<(), String> {
    let Some(path) = path() else { return Ok(()) };
    if !path.exists() {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(&path, DEFAULT);
        return Ok(());
    }
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let config = parse(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    let _ = CONFIG.set(config);
    Ok(())
}

fn path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("sawmill").join("config.toml"))
}

fn regex(src: &str) -> Result<Regex, String> {
    Regex::new(src).map_err(|e| format!("bad regex {src:?}: {}", e.to_string().lines().last().unwrap_or_default().trim()))
}

fn colour(name: &str) -> Result<Color, String> {
    Color::from_str(name).map_err(|_| format!("unknown colour {name:?}"))
}

pub fn parse(text: &str) -> Result<Config, String> {
    let file: File = toml::from_str(text).map_err(|e| e.to_string())?;
    let default = || toml::from_str::<File>(DEFAULT).expect("built-in config parses");

    let masks = file.mask.unwrap_or_else(|| default().mask.unwrap_or_default());
    let masks = masks
        .into_iter()
        .map(|m| Ok(Mask { name: m.name.to_uppercase(), re: regex(&m.regex)? }))
        .collect::<Result<Vec<_>, String>>()?;
    if masks.len() > MAX_MASKS {
        return Err(format!("at most {MAX_MASKS} masks"));
    }

    let macros = file.macros.unwrap_or_else(|| default().macros.unwrap_or_default());
    let macros = macros
        .into_iter()
        .map(|(name, v)| {
            let src = v.as_str().ok_or_else(|| format!("macro {name} must be a string"))?;
            regex(src)?;
            Ok((format!("%{name}%"), src.to_string()))
        })
        .collect::<Result<Vec<_>, String>>()?;

    let highlight = file.highlight.unwrap_or_else(|| default().highlight.unwrap_or_default());
    let highlight = highlight
        .into_iter()
        .map(|h| {
            if let Some(fg) = &h.fg {
                colour(fg)?;
            }
            if let Some(bg) = &h.bg {
                colour(bg)?;
            }
            let mut mods = Modifier::empty();
            for (on, m) in [
                (h.bold, Modifier::BOLD),
                (h.italic, Modifier::ITALIC),
                (h.dim, Modifier::DIM),
                (h.underline, Modifier::UNDERLINED),
            ] {
                if on {
                    mods = mods.union(m);
                }
            }
            Ok(Highlight { re: regex(&h.regex)?, fg: h.fg, bg: h.bg, mods })
        })
        .collect::<Result<Vec<_>, String>>()?;

    let ui = file.ui.map_or_else(Ui::default, |u| Ui {
        theme: u.theme,
        wrap: u.wrap,
        mouse: u.mouse,
        case_sensitive: u.case_sensitive,
        power_scroll: u.power_scroll,
    });

    Ok(Config { masks, macros, highlight, ui })
}

/// The `[ui]` block written back when a setting changes.
pub fn ui_block(ui: &Ui) -> String {
    format!(
        "[ui]\ntheme = \"{}\"\nwrap = {}\nmouse = {}\ncase_sensitive = {}\npower_scroll = {}\n",
        ui.theme, ui.wrap, ui.mouse, ui.case_sensitive, ui.power_scroll
    )
}

/// Splice `block` in as the `[ui]` section, leaving every other line alone.
pub fn replace_ui(text: &str, block: &str) -> String {
    let block = format!("{}\n", block.trim_end_matches('\n'));
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let mut start = None;
    let mut end = None;
    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        let header = trimmed == "[ui]" || trimmed.strip_prefix("[ui]").is_some_and(|rest| rest.trim_start().is_empty() || rest.trim_start().starts_with('#'));
        if start.is_none() {
            if header {
                start = Some(i);
            }
        } else if trimmed.starts_with('[') && !header {
            end = Some(i);
            break;
        }
    }
    let Some(start) = start else {
        let mut out = text.to_string();
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        if !out.ends_with("\n\n") && !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&block);
        return out;
    };
    let end = end.unwrap_or(lines.len());
    let mut out = String::new();
    for line in &lines[..start] {
        out.push_str(line);
    }
    if start > 0 && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&block);
    for line in &lines[end..] {
        out.push_str(line);
    }
    out
}

/// Write `[ui]` into the config file. Tests never touch the real file.
pub fn save_ui(ui: &Ui) {
    if cfg!(test) {
        return;
    }
    let Some(path) = path() else { return };
    let text = std::fs::read_to_string(&path).unwrap_or_else(|_| DEFAULT.to_string());
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path, replace_ui(&text, &ui_block(ui)));
}

/// Placeholders are private-use characters, one per mask, so that a masked
/// `ADDR` and the literal word `ADDR` in a log never group together.
const PLACEHOLDER_BASE: u32 = 0xE000;
const MAX_MASKS: usize = 0x1000;

pub fn placeholder(mask: usize) -> char {
    char::from_u32(PLACEHOLDER_BASE + mask as u32).expect("in the private use area")
}

/// The mask a placeholder character stands for, if it is one.
pub fn mask_of(c: char) -> Option<&'static Mask> {
    let i = (c as u32).checked_sub(PLACEHOLDER_BASE)? as usize;
    get().masks.get(i)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_built_in_config_parses() {
        let c = parse(DEFAULT).unwrap();
        assert_eq!(c.masks.last().unwrap().name, "NUM");
        assert!(c.masks.iter().all(|m| m.name != "PATH"));
        assert!(c.macros.iter().any(|(n, _)| n == "%UUID%"));
        assert_eq!(c.highlight[0].fg.as_deref(), Some("green"));
        assert!(c.highlight.iter().any(|h| h.re.as_str().contains("::") && h.fg.as_deref() == Some("cyan")));
        assert_eq!(c.ui.theme, "terminal");
        assert!(c.ui.mouse);
        assert!(!c.ui.wrap);
    }

    #[test]
    fn a_missing_section_falls_back_to_the_default() {
        let c = parse("[macros]\nHOST = 'web\\d+'\n").unwrap();
        assert_eq!(c.macros, [("%HOST%".to_string(), r"web\d+".to_string())]);
        assert_eq!(c.masks.len(), parse(DEFAULT).unwrap().masks.len());
        assert_eq!(c.ui.theme, "terminal");
        let partial = parse("[ui]\ntheme = 'dracula'\n").unwrap();
        assert_eq!(partial.ui.theme, "dracula");
        assert!(partial.ui.mouse, "a key left out keeps its default");
        assert!(!partial.ui.case_sensitive);
    }

    #[test]
    fn replacing_the_ui_section_leaves_the_rest_of_the_file() {
        let text = "# keep\n[[mask]]\nname = 'pid'\nregex = '\\d+'\n";
        let out = replace_ui(text, &ui_block(&Ui { theme: "nord".into(), wrap: true, ..Ui::default() }));
        assert!(out.contains("name = 'pid'"), "{out}");
        assert!(out.contains("theme = \"nord\""));
        assert!(out.contains("wrap = true"));
        let again = replace_ui(&out, &ui_block(&Ui { theme: "dracula".into(), ..Ui::default() }));
        assert!(again.contains("name = 'pid'"));
        assert!(again.contains("theme = \"dracula\""));
        assert!(!again.contains("theme = \"nord\""));
        assert_eq!(parse(&again).unwrap().ui.theme, "dracula");
    }

    #[test]
    fn bad_entries_are_reported() {
        let err = |text: &str| parse(text).err().unwrap_or_default();
        assert!(err("[[mask]]\nname = 'x'\nregex = '('\n").contains("bad regex"));
        assert!(err("[[highlight]]\nregex = 'x'\nfg = 'mauve'\n").contains("mauve"));
        assert!(parse("[[mask]]\nnmae = 'x'\n").is_err());
    }

    #[test]
    fn mask_names_are_uppercased() {
        let c = parse("[[mask]]\nname = 'pid'\nregex = '\\d+'\n").unwrap();
        assert_eq!(c.masks[0].name, "PID");
    }
}
