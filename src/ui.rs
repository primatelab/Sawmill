use crate::app::{App, Bucket, View};
use crate::parse::Severity;
use ratatui::{
    Frame,
    buffer::Buffer,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, List, ListItem, ListState, Paragraph},
};

const HIST_HEIGHT: u16 = 12;

pub fn colour(s: Severity) -> Color {
    match s {
        Severity::Error => Color::Red,
        Severity::Warn => Color::Yellow,
        Severity::Info => Color::Cyan,
        Severity::Debug => Color::Blue,
        Severity::Other => Color::DarkGray,
    }
}

pub fn draw(f: &mut Frame, app: &mut App) {
    let [hist_area, main_area, help_area] = Layout::vertical([
        Constraint::Length(HIST_HEIGHT),
        Constraint::Min(3),
        Constraint::Length(1),
    ])
    .areas(f.area());

    draw_histogram(f, app, hist_area);
    match app.view {
        View::Patterns => draw_patterns(f, app, main_area),
        View::Lines => draw_lines(f, app, main_area),
    }
    draw_help(f, app, help_area);
}

fn draw_histogram(f: &mut Frame, app: &mut App, area: Rect) {
    let block = Block::bordered().title(" Timeline ".bold());
    let inner = block.inner(area);
    f.render_widget(block, area);

    let buckets = app.histogram(inner.width as usize);
    if buckets.is_empty() {
        f.render_widget(
            Paragraph::new("No timestamps found in this log.").dark_gray(),
            inner,
        );
        return;
    }

    let span = format!(
        " {} \u{2192} {}  ({}s buckets) ",
        buckets[0].start.format("%Y-%m-%d %H:%M:%S"),
        buckets[buckets.len() - 1].start.format("%H:%M:%S"),
        app.bucket_secs
    );
    f.render_widget(
        Block::bordered().title(" Timeline ".bold()).title_bottom(span.dark_gray()),
        area,
    );

    render_bars(&buckets, inner, f.buffer_mut());
}

/// Stacked bars, one column per bucket, most severe at the top of the stack.
fn render_bars(buckets: &[Bucket], area: Rect, buf: &mut Buffer) {
    let peak = buckets.iter().map(|b| b.total()).max().unwrap_or(0);
    if peak == 0 || area.height == 0 {
        return;
    }
    let h = area.height as usize;

    for (i, bucket) in buckets.iter().enumerate().take(area.width as usize) {
        let x = area.x + i as u16;
        let bar = (bucket.total() * h).div_ceil(peak).min(h);

        // Lay the stack out from the bottom, in increasing order of severity.
        let mut rows: Vec<Severity> = Vec::with_capacity(bar);
        for sev in Severity::ALL.iter().rev() {
            let n = (bucket.counts[*sev as usize] * bar).div_ceil(bucket.total().max(1));
            rows.extend(std::iter::repeat_n(*sev, n));
        }
        rows.truncate(bar);

        for (row, sev) in rows.iter().enumerate() {
            let y = area.y + area.height - 1 - row as u16;
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.set_char('\u{2588}').set_fg(colour(*sev));
            }
        }
    }
}

fn draw_patterns(f: &mut Frame, app: &mut App, area: Rect) {
    let title = format!(
        " Patterns  {} unique / {} lines ",
        app.patterns.len(),
        app.total_matching()
    );
    let block = Block::bordered().title(title.bold());

    if app.patterns.is_empty() {
        f.render_widget(
            Paragraph::new("Nothing matches the current filter.")
                .dark_gray()
                .block(block),
            area,
        );
        return;
    }

    let width = block.inner(area).width as usize;
    let items: Vec<ListItem> = app
        .patterns
        .iter()
        .map(|p| {
            let count = format!("{:>7}  ", p.count);
            let label = format!("{:<5} ", p.severity.label());
            let budget = width.saturating_sub(count.len() + label.len());
            ListItem::new(Line::from(vec![
                Span::styled(count, Style::default().fg(Color::White).bold()),
                Span::styled(label, Style::default().fg(colour(p.severity))),
                Span::raw(truncate(&p.text, budget)),
            ]))
        })
        .collect();

    let mut state = ListState::default().with_selected(Some(app.selected));
    f.render_stateful_widget(
        List::new(items)
            .block(block)
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
            .highlight_symbol(""),
        area,
        &mut state,
    );
}

fn draw_lines(f: &mut Frame, app: &mut App, area: Rect) {
    let lines = app.selected_lines();
    let pattern = app
        .selected_pattern()
        .map(|p| p.text.clone())
        .unwrap_or_default();
    let block = Block::bordered()
        .title(format!(" {} raw lines ", lines.len()).bold())
        .title_bottom(truncate(&pattern, area.width.saturating_sub(4) as usize).dark_gray());

    let width = block.inner(area).width as usize;
    let items: Vec<ListItem> = lines
        .iter()
        .map(|e| {
            let no = format!("{:>7}  ", e.line_no);
            let budget = width.saturating_sub(no.len());
            ListItem::new(Line::from(vec![
                Span::styled(no, Style::default().fg(Color::DarkGray)),
                Span::styled(
                    truncate(e.raw.trim(), budget),
                    Style::default().fg(colour(e.severity)),
                ),
            ]))
        })
        .collect();

    let mut state = ListState::default().with_selected(Some(app.line_scroll));
    f.render_stateful_widget(
        List::new(items)
            .block(block)
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED)),
        area,
        &mut state,
    );
}

fn draw_help(f: &mut Frame, app: &App, area: Rect) {
    let filter = match app.filter {
        None => "all".to_string(),
        Some(s) => s.label().to_lowercase(),
    };
    let keys = match app.view {
        View::Patterns => "\u{2191}\u{2193} select   \u{21b5} drill in   f filter   q quit",
        View::Lines => "\u{2191}\u{2193} scroll   esc back   f filter   q quit",
    };
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(format!(" {} ", app.source), Style::default().fg(Color::Black).bg(Color::Cyan)),
            Span::styled(format!(" {} ", filter), Style::default().fg(Color::Black).bg(Color::White)),
            Span::raw("  "),
            Span::styled(keys, Style::default().fg(Color::DarkGray)),
        ])),
        area,
    );
}

fn truncate(s: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    if s.chars().count() <= width {
        return s.to_string();
    }
    let kept: String = s.chars().take(width.saturating_sub(1)).collect();
    format!("{kept}\u{2026}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_leaves_short_strings_alone() {
        assert_eq!(truncate("abc", 10), "abc");
    }

    #[test]
    fn truncate_marks_elision() {
        assert_eq!(truncate("abcdef", 4), "abc\u{2026}");
    }

    #[test]
    fn truncate_handles_no_space() {
        assert_eq!(truncate("abc", 0), "");
    }

    #[test]
    fn truncate_counts_characters_not_bytes() {
        assert_eq!(truncate("\u{e9}\u{e9}\u{e9}", 3), "\u{e9}\u{e9}\u{e9}");
    }
}
