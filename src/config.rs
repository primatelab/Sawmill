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

pub const DEFAULT: &str = r##"# Sawmill - Dissecting Logs.
# Delete a section to get the built-in default for it back.

############# MASKS ##############

# Masks collapse the variable parts of a line so that similar lines group
# into one pattern. They run in order, so put specific shapes before general
# ones. The name is shown struck through, as written, in place of the match.

[[mask]]
name = "UUID"
regex = '\b[0-9A-Fa-f]{8}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{12}\b'

# Before IPv4, so an IPv4-mapped address is one token. A digit or a 4-digit
# hextet keeps this off clock times and off C++ names such as std::vector.
[[mask]]
name = "IPv6"
builtin = "IP6"

[[mask]]
name = "IPv4"
builtin = "IP4"

# Dates before the bare-number rule, one shape each: ISO, Common Log,
# day-month, month-day, numeric, then a bare clock time.
[[mask]] # Built-in date-parsing regexes.
name = "DATE"
builtin = "DATE"

[[mask]]
name = "TIME"
regex = '\b\d{2}:\d{2}:\d{2}(\.\d+)?\b'

[[mask]]
name = "HEX"
regex = '\b0x[0-9a-fA-F]+\b'

[[mask]]
name = "HASH"
regex = '\b[0-9a-fA-F]{12,}\b'

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

################# MACROS ##################

# Macros expand inside any regex you type:
# %IP4%, %IP6% and %DATE% are builtins.
# %FIND% is a special builtin and stands in for the current find term.

[macros]
UUID = '\b[0-9A-Fa-f]{8}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{12}\b'
OBJECT = '\b[A-Za-z0-9_-]+(::[A-Za-z0-9_-]+)+'

################## HIGHLIGHTS #############

# Highlight rules colour raw lines. Later rules paint over earlier ones.
# fg and bg take a colour name (red, lightred, darkgray, ...), "#rrggbb" or a
# 256-colour index. bold, italic, dim and underline are true or false.

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
regex = '[~.]?/[A-Za-z_./]*/[A-Za-z0-9_./-]*'
fg = "yellow"
dim = true

[[highlight]] # Built-in date-parsing regexes.
builtin = "DATE"
fg = "white"
italic = true

[[highlight]] # Timestamp
regex = '\b[0-2][0-9]:[0-5][0-9]:[0-6][0-9](?:[.,][0-9]+)?\b'
fg = "white"
italic = true

[[highlight]] # IPv4 Address
builtin = "IP4"
fg = "lightred"
bold = true

[[highlight]] # IPv6 Address
builtin = "IP6"
fg = "lightred"
bold = true

[[highlight]] # UUID
regex = '\b[0-9A-Fa-f]{8}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{12}\b'
fg = "cyan"

################# UI SETTINGS ###############

[ui]
theme = "terminal"
wrap = false
mouse = true
case_sensitive = false
power_scroll = false
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
    regex: Option<String>,
    builtin: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HighlightSpec {
    regex: Option<String>,
    builtin: Option<String>,
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

/// IPv4 and an optional port. No leading word boundary.
const IP4: &str = r"\d{1,3}\.\d{1,3}\.\d{1,3}\.\d{1,3}(:\d+)?\b";

/// IPv6, including compressed and IPv4-mapped forms. A digit or a 4-digit
/// hextet keeps this off clock times and off `std::vector`.
const IP6: &str = r"(?i)(?:[0-9a-f]{1,4}:){7}[0-9a-f]{1,4}|::(?:ffff:)?(?:\d{1,3}\.){3}\d{1,3}|(?:[0-9a-f]{1,4}:){1,6}(?:\d{1,3}\.){3}\d{1,3}|(?:[0-9a-f]{1,4}:){0,6}[0-9a-f]*\d[0-9a-f]*::[0-9a-f:]*(?::[0-9a-f]{0,4})*|::[0-9a-f:]*\d[0-9a-f:]*(?::[0-9a-f]{0,4})*|(?:[0-9a-f]{1,4}:){0,6}[0-9a-f]{4}::[0-9a-f]{0,4}(?::[0-9a-f]{0,4})*|::[0-9a-f]{4}";

const BUILTIN_MACROS: [&str; 3] = ["IP4", "IP6", "DATE"];

/// `DATE` is the five date shapes, in the order a mask should try them.
/// ISO comes first so `2026-10-16` is not also read as a numeric date.
/// `IP4` and `IP6` are one pattern each.
fn builtin_regexes(name: &str) -> Result<Vec<String>, String> {
    match name.to_ascii_uppercase().as_str() {
        "DATE" => {
            let month = "Jan(?:uary)?|Feb(?:ruary)?|Mar(?:ch)?|Apr(?:il)?|May|Jun(?:e)?|Jul(?:y)?|Aug(?:ust)?|Sep(?:t(?:ember)?)?|Oct(?:ober)?|Nov(?:ember)?|Dec(?:ember)?";
            Ok(vec![
                r"\b\d{4}[-/]\d{2}[-/]\d{2}(?:[T ]\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:?\d{2})?)?\b".into(),
                format!(r"\b\d{{2}}/(?:{month})/\d{{4}}:\d{{2}}:\d{{2}}:\d{{2}}(?:\.\d+)?(?:\s*[+-]\d{{4}})?\b"),
                format!(r"\b\d{{1,2}}\s+(?:{month})\s+\d{{4}}(?:[ T]\d{{2}}:\d{{2}}:\d{{2}})?\b"),
                format!(r"\b(?:{month})\s+\d{{1,2}},?(?:\s+\d{{4}})?(?:\s+\d{{2}}:\d{{2}}:\d{{2}})?\b"),
                r"\b\d{1,2}[-/]\d{1,2}[-/]\d{2,4}(?:[ T]\d{2}:\d{2}:\d{2})?\b".into(),
            ])
        }
        "IP4" => Ok(vec![IP4.to_string()]),
        "IP6" => Ok(vec![IP6.to_string()]),
        _ => Err(format!("unknown builtin {name:?}")),
    }
}

/// One atom, so a builtin can sit inside a larger regex. Several shapes
/// become one alternation. A macro of the same name in the file wins.
fn add_builtin_macros(macros: &mut Vec<(String, String)>) -> Result<(), String> {
    for name in BUILTIN_MACROS {
        let key = format!("%{name}%");
        if macros.iter().any(|(n, _)| n.eq_ignore_ascii_case(&key)) {
            continue;
        }
        let src = format!("(?:{})", builtin_regexes(name)?.join("|"));
        regex(&src)?;
        macros.push((key, src));
    }
    Ok(())
}

/// One written rule, or the patterns a builtin stands for. A rule is one or the other.
fn sources(regex: Option<&str>, builtin: Option<&str>, what: &str) -> Result<Vec<String>, String> {
    match (regex, builtin) {
        (Some(re), None) => Ok(vec![re.to_string()]),
        (None, Some(name)) => builtin_regexes(name),
        (Some(_), Some(_)) => Err(format!("{what}: give a regex or a builtin, not both")),
        (None, None) => Err(format!("{what}: needs a regex or a builtin")),
    }
}

fn colour(name: &str) -> Result<Color, String> {
    Color::from_str(name).map_err(|_| format!("unknown colour {name:?}"))
}

pub fn parse(text: &str) -> Result<Config, String> {
    let file: File = toml::from_str(text).map_err(|e| e.to_string())?;
    let default = || toml::from_str::<File>(DEFAULT).expect("built-in config parses");

    let masks = file.mask.unwrap_or_else(|| default().mask.unwrap_or_default());
    let mut out = Vec::new();
    for m in masks {
        let name = m.name;
        for src in sources(m.regex.as_deref(), m.builtin.as_deref(), &format!("mask {name}"))? {
            out.push(Mask { name: name.clone(), re: regex(&src)? });
        }
    }
    let masks = out;
    if masks.len() > MAX_MASKS {
        return Err(format!("at most {MAX_MASKS} masks"));
    }

    let macros = file.macros.unwrap_or_else(|| default().macros.unwrap_or_default());
    let mut macros = macros
        .into_iter()
        .map(|(name, v)| {
            let src = v.as_str().ok_or_else(|| format!("macro {name} must be a string"))?;
            regex(src)?;
            Ok((format!("%{name}%"), src.to_string()))
        })
        .collect::<Result<Vec<_>, String>>()?;
    add_builtin_macros(&mut macros)?;

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
            let srcs = sources(h.regex.as_deref(), h.builtin.as_deref(), "highlight rule")?;
            srcs.into_iter()
                .map(|src| Ok(Highlight { re: regex(&src)?, fg: h.fg.clone(), bg: h.bg.clone(), mods }))
                .collect::<Result<Vec<_>, _>>()
        })
        .collect::<Result<Vec<_>, String>>()?
        .into_iter()
        .flatten()
        .collect();

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
/// `IPv4` and the literal word `IPv4` in a log never group together.
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
        assert_eq!(c.masks.iter().filter(|m| m.name == "DATE").count(), 5);
        assert!(c.masks.iter().any(|m| m.name == "DATE" && m.re.as_str().contains(r"\d{1,2}[-/]\d{1,2}")));
        assert!(c.masks.iter().any(|m| m.name == "TIME"));
        assert!(c.highlight.iter().any(|h| h.re.as_str().contains("ober")));
        assert!(c.masks.iter().any(|m| m.name == "IPv4" && m.re.as_str() == IP4));
        assert!(c.masks.iter().any(|m| m.name == "IPv6" && m.re.as_str() == IP6));
        assert!(c.macros.iter().any(|(n, _)| n == "%UUID%"));
        assert!(c.macros.iter().any(|(n, s)| n == "%OBJECT%" && s.contains("::")));
        assert!(c.macros.iter().any(|(n, s)| n == "%IP4%" && s.starts_with("(?:") && s.contains(r"\d{1,3}")));
        assert!(c.macros.iter().any(|(n, s)| n == "%IP6%" && s.starts_with("(?:")));
        assert!(c.macros.iter().any(|(n, s)| n == "%DATE%" && s.starts_with("(?:")));
        assert_eq!(c.highlight[0].fg.as_deref(), Some("green"));
        assert!(c.highlight.iter().any(|h| h.re.as_str().contains("::") && h.fg.as_deref() == Some("cyan")));
        assert_eq!(c.ui.theme, "terminal");
        assert!(c.ui.mouse);
        assert!(!c.ui.wrap);
    }

    #[test]
    fn a_missing_section_falls_back_to_the_default() {
        let c = parse("[macros]\nHOST = 'web\\d+'\n").unwrap();
        assert!(c.macros.iter().any(|(n, s)| n == "%HOST%" && s == r"web\d+"));
        assert!(!c.macros.iter().any(|(n, _)| n == "%UUID%"), "a present section replaces the default macros");
        for name in ["%IP4%", "%IP6%", "%DATE%"] {
            assert!(c.macros.iter().any(|(n, _)| n == name), "{name} is a builtin macro");
        }
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
    fn mask_names_are_kept_as_written() {
        let c = parse("[[mask]]\nname = 'hostname=HOSTNAME-'\nregex = 'x'\n").unwrap();
        assert_eq!(c.masks[0].name, "hostname=HOSTNAME-");
    }

    #[test]
    fn a_date_builtin_expands_to_the_five_shapes() {
        let c = parse("[[mask]]\nname = 'date'\nbuiltin = 'DATE'\n[[highlight]]\nbuiltin = 'date'\nfg = 'white'\nitalic = true\n").unwrap();
        assert_eq!(c.masks.len(), 5);
        assert!(c.masks.iter().all(|m| m.name == "date"));
        assert_eq!(c.highlight.len(), 5);
        let painted: Vec<_> = c.highlight.iter().map(|h| h.re.as_str()).collect();
        let masked: Vec<_> = c.masks.iter().map(|m| m.re.as_str()).collect();
        assert_eq!(painted, masked);
        assert!(c.highlight.iter().all(|h| h.mods.contains(Modifier::ITALIC) && h.fg.as_deref() == Some("white")));
        let err = |text: &str| parse(text).err().unwrap_or_default();
        assert!(err("[[mask]]\nname = 'x'\n").contains("regex or a builtin"));
        assert!(err("[[mask]]\nname = 'x'\nregex = 'a'\nbuiltin = 'DATE'\n").contains("not both"));
        assert!(err("[[highlight]]\nbuiltin = 'NOPE'\n").contains("NOPE"));
    }

    #[test]
    fn ip_builtins_expand_and_are_macros() {
        let c = parse("[[mask]]\nname = 'v4'\nbuiltin = 'ip4'\n[[mask]]\nname = 'v6'\nbuiltin = 'IP6'\n[[highlight]]\nbuiltin = 'IP4'\nfg = 'red'\n").unwrap();
        assert_eq!(c.masks.len(), 2);
        assert_eq!(c.masks[0].re.as_str(), IP4);
        assert_eq!(c.masks[1].re.as_str(), IP6);
        assert_eq!(c.highlight[0].re.as_str(), IP4);
        let macro_of = |name: &str| c.macros.iter().find(|(n, _)| n == name).unwrap().1.as_str();
        let ip4 = Regex::new(macro_of("%IP4%")).unwrap();
        assert!(ip4.is_match("10.0.0.1"));
        assert!(ip4.is_match("10.0.0.1:22"));
        let ip6 = Regex::new(macro_of("%IP6%")).unwrap();
        assert!(ip6.is_match("2001:db8::1"));
        assert!(ip6.is_match("::ffff:192.0.2.1"));
        assert!(!ip6.is_match("12:34:56"));
        assert!(!ip6.is_match("std::vector"));
        let date = Regex::new(macro_of("%DATE%")).unwrap();
        assert!(date.is_match("2026-10-16"));
        assert!(date.is_match("16/Oct/2026:09:21:29"));
        assert!(date.is_match("Oct 16 09:21:29"));
    }

    #[test]
    fn a_written_macro_hides_the_builtin_of_the_same_name() {
        let c = parse("[macros]\nip4 = 'custom'\n").unwrap();
        let found: Vec<_> = c.macros.iter().filter(|(n, _)| n.eq_ignore_ascii_case("%ip4%")).map(|(_, s)| s.as_str()).collect();
        assert_eq!(found, ["custom"]);
        assert!(c.macros.iter().any(|(n, _)| n == "%IP6%"));
        assert!(c.macros.iter().any(|(n, _)| n == "%DATE%"));
    }
}
