//! Colouring for raw lines and pattern text. Rules paint in order and a later
//! match restyles what an earlier one coloured, so search hits sit on top.

use crate::config;
use crate::parse::{self, LogKind, Severity};
use crate::theme::{Theme, ink};
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;
use regex::Regex;

fn base(theme: &Theme) -> Style {
    Style::new().fg(theme.text)
}

fn include(theme: &Theme) -> Style {
    Style::new().fg(theme.bright).add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
}

fn find(theme: &Theme) -> Style {
    Style::new().fg(ink(theme.find_bg)).bg(theme.find_bg).add_modifier(Modifier::BOLD)
}

fn select(theme: &Theme) -> Style {
    Style::new().fg(ink(theme.select_bg)).bg(theme.select_bg)
}

fn mask(theme: &Theme) -> Style {
    Style::new().fg(theme.dim).add_modifier(Modifier::CROSSED_OUT)
}

fn level(s: Severity, theme: &Theme) -> Style {
    Style::new().fg(theme.severity(s)).add_modifier(Modifier::BOLD)
}

/// One style per byte, collapsed into spans at the end.
struct Layers {
    text: String,
    styles: Vec<Style>,
}

impl Layers {
    fn new(text: &str, theme: &Theme) -> Self {
        let text = text.replace('\t', " ");
        let styles = vec![base(theme); text.len()];
        Layers { text, styles }
    }

    fn range(&mut self, r: std::ops::Range<usize>, style: Style) {
        for s in &mut self.styles[r] {
            *s = s.patch(style);
        }
    }

    /// Replace the style outright, so a mask drops any colour a highlight left.
    fn assign(&mut self, r: std::ops::Range<usize>, style: Style) {
        for s in &mut self.styles[r] {
            *s = style;
        }
    }

    fn paint(&mut self, re: &Regex, style: Style) {
        let ranges: Vec<_> = re.find_iter(&self.text).map(|m| m.range()).collect();
        for r in ranges {
            self.range(r, style);
        }
    }

    /// Paint a range given in characters rather than bytes.
    fn chars(&mut self, r: Option<std::ops::Range<usize>>, style: Style) {
        let Some(r) = r else { return };
        let byte = |c: usize| self.text.char_indices().nth(c).map_or(self.text.len(), |(b, _)| b);
        let (lo, hi) = (byte(r.start), byte(r.end));
        self.range(lo..hi, style);
    }

    fn level(&mut self, kind: LogKind, theme: &Theme) {
        let token = match kind {
            LogKind::Web => parse::http_status(&self.text),
            LogKind::System => parse::severity_token(&self.text),
        };
        if let Some((r, s)) = token {
            self.range(r, level(s, theme));
        }
    }

    fn spans(self) -> Vec<Span<'static>> {
        let mut out = Vec::new();
        let mut start = 0;
        for i in 1..=self.text.len() {
            let changed = i == self.text.len() || self.styles[i] != self.styles[start];
            if changed && self.text.is_char_boundary(i) {
                out.push(Span::styled(self.text[start..i].to_string(), self.styles[start]));
                start = i;
            }
        }
        out
    }
}

/// `select` is a character range picked with the mouse; it paints last.
pub fn raw(
    line: &str,
    include_re: Option<&Regex>,
    find_re: Option<&Regex>,
    select_at: Option<std::ops::Range<usize>>,
    kind: LogKind,
    theme: &Theme,
) -> Vec<Span<'static>> {
    let mut l = Layers::new(line, theme);
    for rule in &config::get().highlight {
        l.paint(&rule.re, rule.style(theme));
    }
    l.level(kind, theme);
    if let Some(re) = include_re {
        l.paint(re, include(theme));
    }
    if let Some(re) = find_re {
        l.paint(re, find(theme));
    }
    l.chars(select_at, select(theme));
    l.spans()
}

/// `masks` are byte ranges of mask names in `text`, from `parse::display_pattern`.
/// Highlight rules paint the same way as on a raw line. A mask name then replaces
/// that style, so `NUM` stays struck through even when a rule also matches it.
pub fn pattern(
    text: &str,
    masks: &[std::ops::Range<usize>],
    find_re: Option<&Regex>,
    select_at: Option<std::ops::Range<usize>>,
    kind: LogKind,
    theme: &Theme,
) -> Vec<Span<'static>> {
    let mut l = Layers::new(text, theme);
    for rule in &config::get().highlight {
        l.paint(&rule.re, rule.style(theme));
    }
    if let Some((r, s)) = match kind {
        LogKind::Web => parse::http_status(&l.text),
        LogKind::System => parse::severity_token(&l.text),
    } {
        let covered = masks.iter().any(|m| m.start < r.end && r.start < m.end);
        if !covered {
            l.range(r, level(s, theme));
        }
    }
    for r in masks {
        l.assign(r.clone(), mask(theme));
    }
    if let Some(re) = find_re {
        l.paint(re, find(theme));
    }
    l.chars(select_at, select(theme));
    l.spans()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme;
    use ratatui::style::Color;

    fn style_of<'a>(spans: &'a [Span], text: &str) -> &'a Style {
        &spans.iter().find(|s| s.content == text).unwrap_or_else(|| panic!("no span {text:?} in {spans:?}")).style
    }

    fn raw(line: &str, include: Option<&Regex>, find: Option<&Regex>, select: Option<std::ops::Range<usize>>, kind: LogKind) -> Vec<Span<'static>> {
        super::raw(line, include, find, select, kind, &theme::TERMINAL)
    }

    fn pattern(text: &str, masks: &[std::ops::Range<usize>], find: Option<&Regex>, select: Option<std::ops::Range<usize>>, kind: LogKind) -> Vec<Span<'static>> {
        super::pattern(text, masks, find, select, kind, &theme::TERMINAL)
    }

    #[test]
    fn colours_the_level_token_that_set_the_severity() {
        let spans = raw("Feb 15 host app : [ERR] boom", None, None, None, LogKind::System);
        assert_eq!(style_of(&spans, "ERR").fg, Some(Color::Red));
    }

    #[test]
    fn colours_the_status_code_on_a_web_log() {
        let line = r#"1.2.3.4 - - [20/Oct/2026:09:00:00 +0000] "GET /a HTTP/1.1" 500 1"#;
        let spans = raw(line, None, None, None, LogKind::Web);
        assert_eq!(style_of(&spans, "500").fg, Some(Color::Red));
    }

    #[test]
    fn a_find_hit_sits_on_top_of_syntax_colour() {
        let find = Regex::new("10.0").unwrap();
        let spans = raw("conn 10.0.0.1 failed", None, Some(&find), None, LogKind::System);
        assert_eq!(style_of(&spans, "10.0").bg, Some(Color::LightGreen));
        assert_eq!(style_of(&spans, ".0.1").fg, Some(Color::LightRed));
    }

    #[test]
    fn spans_rebuild_the_line_exactly() {
        let line = "2026-10-16 09:00:00 [WARN] caf\u{e9} /var/log {x}";
        let joined: String = raw(line, None, None, None, LogKind::System).iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(joined, line);
    }

    #[test]
    fn a_selection_counts_characters_and_paints_over_everything() {
        let find = Regex::new("caf").unwrap();
        let spans = raw("\u{e9}\u{e9} caf\u{e9} x", None, Some(&find), Some(3..7), LogKind::System);
        assert_eq!(style_of(&spans, "caf").bg, Some(Color::LightBlue), "over the find hit");
        assert_eq!(style_of(&spans, "\u{e9}").bg, Some(Color::LightBlue));
        assert_eq!(style_of(&spans, "\u{e9}\u{e9} ").bg, None);
    }

    #[test]
    fn placeholders_are_struck_through_in_patterns() {
        let masks = [std::ops::Range { start: 12, end: 15 }];
        let spans = pattern("[INFO] took NUMs", &masks, None, None, LogKind::System);
        assert!(style_of(&spans, "NUM").add_modifier.contains(Modifier::CROSSED_OUT));
        assert!(!style_of(&spans, "s").add_modifier.contains(Modifier::CROSSED_OUT));
    }

    #[test]
    fn a_mask_replaces_a_highlight_that_covers_it() {
        let masks = [std::ops::Range { start: 5, end: 8 }];
        let spans = pattern("foo::NUM", &masks, None, None, LogKind::System);
        assert_eq!(style_of(&spans, "foo::").fg, Some(Color::Cyan));
        let num = style_of(&spans, "NUM");
        assert!(num.add_modifier.contains(Modifier::CROSSED_OUT));
        assert_eq!(num.fg, Some(Color::DarkGray));
    }

    #[test]
    fn a_named_highlight_follows_the_theme() {
        let theme = theme::by_name("dracula");
        let spans = super::raw("fn main() {", None, None, None, LogKind::System, theme);
        assert_eq!(style_of(&spans, "{").fg, Some(Color::Rgb(0x50, 0xfa, 0x7b)));
    }
}
