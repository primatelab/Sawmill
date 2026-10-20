use crate::app::{allocate, App, Axis, Bar, Bucket, HistGeom, Menu, RowHit, ScreenBox, View};
use crate::buzz;
use crate::config;
use crate::highlight;
use crate::parse::{self, LogKind, Severity};
use crate::theme::{self, Theme, ink};
use ratatui::{
    Frame,
    buffer::Buffer,
    layout::{Alignment, Constraint, Flex, Layout, Margin, Rect},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span, Text},
    widgets::{
        Block, Clear, Gauge, List, ListItem, ListState, Padding, Paragraph, Scrollbar, ScrollbarOrientation,
        ScrollbarState,
    },
};
use std::collections::VecDeque;

const HIST_HEIGHT: u16 = 12;
const LOWER_HALF: char = '\u{2584}';
/// Sits between the count, level or line number and the text.
const DIVIDER: &str = " \u{2502} ";
use parse::OCTAL_NL;
/// Longer names are cut so the file column cannot crowd out the line.
const MAX_FILE_NAME: usize = 24;
/// Status-bar glyphs. On a web log, `3` and `2` share the 2xx/3xx filter.
fn level_marks(kind: LogKind) -> &'static [(&'static str, Severity)] {
    match kind {
        LogKind::System => &[("E", Severity::Error), ("W", Severity::Warn), ("I", Severity::Info), ("D", Severity::Debug), ("O", Severity::Other)],
        LogKind::Web => &[
            ("5", Severity::Error),
            ("4", Severity::Warn),
            ("3", Severity::Info),
            ("2", Severity::Info),
            ("1", Severity::Debug),
            ("0", Severity::Other),
        ],
    }
}

fn theme_of(app: &App) -> &'static Theme {
    theme::by_name(&app.theme_name)
}

fn cursor_style(theme: &Theme) -> Style {
    Style::new().bg(theme.cursor).add_modifier(Modifier::BOLD)
}

/// Paint the theme background. The terminal theme leaves the frame alone.
fn fill_bg(f: &mut Frame, area: Rect, theme: &Theme) {
    if theme.bg != Color::Reset {
        f.render_widget(Block::default().style(Style::new().bg(theme.bg).fg(theme.text)), area);
    }
}

pub fn draw(f: &mut Frame, app: &mut App) {
    fill_bg(f, f.area(), theme_of(app));
    let [hist_area, main_area, status_area] = Layout::vertical([
        Constraint::Length(HIST_HEIGHT),
        Constraint::Min(3),
        Constraint::Length(1),
    ])
    .areas(f.area());

    app.rows.clear();
    app.vbar = None;
    app.hbar = None;
    draw_histogram(f, app, hist_area);
    match app.view {
        View::Patterns => draw_patterns(f, app, main_area),
        View::Lines | View::All => draw_lines(f, app, main_area),
    }
    draw_status(f, app, status_area);
    if let Some(m) = &app.menu {
        draw_menu(f, m, theme_of(app));
    }
    if app.show_help {
        draw_help(f, theme_of(app));
    }
}

/// Progress while a log loads. `total` is the file size when it is known;
/// for a gzip it is the compressed size, as is `done`.
pub fn draw_loading(f: &mut Frame, source: &str, done: u64, total: Option<u64>, grouping: bool) {
    let theme = theme::by_name(&config::ui().theme);
    fill_bg(f, f.area(), theme);
    let mb = |b: u64| b as f64 / 1_048_576.0;
    let saw_h = buzz::HEIGHT as u16;
    let gauge_h = 3;
    let show_saw = f.area().height >= saw_h + gauge_h;
    let [band] = Layout::horizontal([Constraint::Percentage(60)]).flex(Flex::Center).areas(f.area());
    let [band] = Layout::vertical([Constraint::Length(if show_saw { saw_h + gauge_h } else { gauge_h })])
        .flex(Flex::Center)
        .areas(band);
    let gauge_area = if show_saw {
        let [saw_area, gauge_area] =
            Layout::vertical([Constraint::Length(saw_h), Constraint::Length(gauge_h)]).areas(band);
        let saw = Paragraph::new(buzz::picture(buzz::frame_index())).alignment(Alignment::Center);
        f.render_widget(saw, saw_area);
        gauge_area
    } else {
        band
    };
    let block = Block::bordered().title(format!(" Loading {source} ").bold());
    let (ratio, label) = match (grouping, total) {
        (true, _) => (1.0, "grouping patterns\u{2026}".to_string()),
        (false, Some(t)) if t > 0 => {
            let r = (done as f64 / t as f64).min(1.0);
            (r, format!("{:.0}%  {:.1} / {:.1} MB", r * 100.0, mb(done), mb(t)))
        }
        _ => (0.0, format!("{:.1} MB read", mb(done))),
    };
    f.render_widget(
        Gauge::default()
            .block(block)
            .gauge_style(Style::default().fg(theme.accent).bg(theme.gauge))
            .ratio(ratio)
            .label(label),
        gauge_area,
    );
}

fn draw_menu(f: &mut Frame, m: &Menu, theme: &Theme) {
    let area = Rect::new(m.x, m.y, m.width(), m.height()).intersection(f.area());
    let items: Vec<ListItem> = m
        .entries
        .iter()
        .enumerate()
        .map(|(i, (s, _))| {
            let item = ListItem::new(format!(" {s}"));
            if m.selectable(i) { item } else { item.style(Style::default().fg(theme.dim)) }
        })
        .collect();
    let mut state = ListState::default().with_selected(Some(m.at));
    let mut block = Block::bordered().border_style(Style::default().fg(theme.accent));
    if theme.bg != Color::Reset {
        block = block.style(Style::new().bg(theme.bg).fg(theme.text));
    }
    if let Some(t) = m.title {
        block = block.title(format!(" {t} ").bold());
    }
    f.render_widget(Clear, area);
    fill_bg(f, area, theme);
    f.render_stateful_widget(
        List::new(items)
            .block(block)
            .highlight_style(Style::default().fg(ink(theme.accent)).bg(theme.accent)),
        area,
        &mut state,
    );
}

/// The bucket under the pointer: its time span and how many lines fall in it.
fn draw_tooltip(f: &mut Frame, app: &App, buckets: &[Bucket], inner: Rect, theme: &Theme) {
    if app.menu.is_some() {
        return;
    }
    let (Some(g), Some((col, row))) = (app.hist_geom, app.hover) else { return };
    if !g.contains(col, row) {
        return;
    }
    let b = &buckets[g.bucket_at(col)];
    let end = b.start + chrono::TimeDelta::seconds(g.secs);
    let multi_day = buckets[0].start.date() != buckets[buckets.len() - 1].start.date();
    let fmt = if multi_day { "%Y-%m-%d %H:%M:%S" } else { "%H:%M:%S" };
    let text = format!(
        " {} \u{2013} {}  {} lines ",
        b.start.format(fmt),
        end.format("%H:%M:%S"),
        b.total()
    );
    let width = (text.chars().count() as u16).min(f.area().width);
    let x = (col + 2).min(f.area().width.saturating_sub(width));
    let y = if row + 1 < inner.bottom() { row + 1 } else { row.saturating_sub(1) };
    let area = Rect::new(x, y, width, 1);
    f.render_widget(Clear, area);
    f.render_widget(
        Paragraph::new(text).style(Style::default().fg(ink(theme.bright)).bg(theme.bright)),
        area,
    );
}

/// Screen rows for the items `render_window` drew. `firsts` are the character
/// offsets each visual row of an item starts at, drawn at column `x`.
fn row_hits(inner: Rect, drawn: &[(usize, Vec<usize>)], x: u16) -> Vec<RowHit> {
    let mut out = Vec::new();
    let mut y = inner.y;
    for (pos, firsts) in drawn {
        for &first in firsts {
            if y >= inner.bottom() {
                return out;
            }
            out.push(RowHit { y, x, pos: *pos, first });
            y += 1;
        }
    }
    out
}

fn divider(theme: &Theme) -> Span<'static> {
    Span::styled(DIVIDER, Style::default().fg(theme.dim))
}

/// Vertical bar on the right border, and a horizontal one on the reserved row
/// under `inner` when the text is wider than the view. Records both on `app`
/// so the mouse can drag them.
#[allow(clippy::too_many_arguments)]
fn paint_scrollbars(
    f: &mut Frame,
    app: &mut App,
    area: Rect,
    inner: Rect,
    len: usize,
    top: usize,
    shown: usize,
    wide: Option<(usize, usize, usize)>,
) {
    let overflows = len > 0 && (top > 0 || top + shown < len);
    if overflows {
        let mut state = ScrollbarState::new(len).position(top).viewport_content_length(shown.max(1));
        let track = area.inner(Margin { vertical: 1, horizontal: 0 });
        f.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .thumb_style(Style::default().fg(theme_of(app).accent))
                .track_style(Style::default().fg(theme_of(app).dim)),
            track,
            &mut state,
        );
        app.vbar = Some(Bar {
            axis: Axis::Vertical,
            x: track.right().saturating_sub(1),
            y: track.y,
            cells: track.height,
            max: len - 1,
        });
    }
    if let Some((content, pos, view)) = wide.filter(|&(content, _, view)| view > 0 && content > view) {
        let mut state = ScrollbarState::new(content)
            .position(pos.min(content - view))
            .viewport_content_length(view);
        let track = Rect { x: inner.x, y: inner.bottom(), width: inner.width, height: 1 };
        f.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::HorizontalBottom)
                .thumb_style(Style::default().fg(theme_of(app).accent))
                .track_style(Style::default().fg(theme_of(app).dim)),
            track,
            &mut state,
        );
        app.hbar = Some(Bar { axis: Axis::Horizontal, x: track.x, y: track.y, cells: track.width, max: content - view });
    }
}

fn draw_histogram(f: &mut Frame, app: &mut App, area: Rect) {
    let theme = theme_of(app);
    app.hist_frame = Some(ScreenBox { x: area.x, y: area.y, width: area.width, height: area.height });
    let title = if app.time.is_some() { " Timeline (time filtered) " } else { " Timeline " };
    let block = Block::bordered().title(title.bold());
    let inner = block.inner(area);

    let buckets = app.histogram(inner.width as usize).to_vec();
    if buckets.is_empty() {
        app.hist_geom = None;
        f.render_widget(
            Paragraph::new("No timestamps match the current filters.")
                .style(Style::new().fg(theme.dim))
                .block(block),
            area,
        );
        return;
    }

    let first = buckets[0].start;
    let last = buckets[buckets.len() - 1].start;
    let end = if last.date() == first.date() {
        last.format("%H:%M:%S")
    } else {
        last.format("%Y-%m-%d %H:%M:%S")
    };
    let span = format!(
        " {} \u{2192} {}  ({}s buckets) ",
        first.format("%Y-%m-%d %H:%M:%S"),
        end,
        app.bucket_secs
    );
    f.render_widget(block.title_bottom(span.fg(theme.dim)), area);

    let drag = app.drag.map(|(a, b)| (a.min(b), a.max(b)));
    let secs = app.bucket_secs.max(1);
    let cursor = app
        .cursor_time()
        .filter(|&t| t >= first)
        .map(|t| ((t - first).num_seconds() / secs) as usize);
    let lit = |i: usize| drag.is_some_and(|(lo, hi)| i >= lo && i <= hi) || cursor == Some(i);
    render_bars(&buckets, inner, f.buffer_mut(), theme, lit);
    app.hist_geom = Some(HistGeom {
        x: inner.x,
        y: inner.y,
        height: inner.height,
        len: buckets.len().min(inner.width as usize),
        start: first,
        secs: app.bucket_secs,
    });
    draw_tooltip(f, app, &buckets, inner, theme);
}

/// Stacked bars at two units per cell: `▄` paints the lower unit in the
/// foreground and the upper one in the background. Most severe on top.
/// Columns where `lit` holds are drawn in lighter colours on grey.
fn render_bars(buckets: &[Bucket], area: Rect, buf: &mut Buffer, theme: &Theme, lit: impl Fn(usize) -> bool) {
    let peak = buckets.iter().map(|b| b.total()).max().unwrap_or(0);
    if peak == 0 || area.height == 0 {
        return;
    }
    let h = area.height as usize;
    let units = h * 2;
    let shade = theme.shade;

    for (i, bucket) in buckets.iter().enumerate().take(area.width as usize) {
        let x = area.x + i as u16;
        let selected = lit(i);
        let empty = if selected { shade } else { Color::Reset };
        let bar = (bucket.total() * units).div_ceil(peak).min(units);
        let shares = allocate(&bucket.counts, bar);

        let mut stack: Vec<Color> = Vec::with_capacity(bar);
        for sev in Severity::ALL.iter().rev() {
            let c = if selected { theme.severity_lit(*sev) } else { theme.severity(*sev) };
            stack.extend(std::iter::repeat_n(c, shares[*sev as usize]));
        }

        for row in 0..h {
            let y = area.y + area.height - 1 - row as u16;
            let Some(cell) = buf.cell_mut((x, y)) else { continue };
            match (stack.get(row * 2), stack.get(row * 2 + 1)) {
                (Some(&lower), upper) => {
                    cell.set_char(LOWER_HALF).set_fg(lower).set_bg(upper.copied().unwrap_or(empty));
                }
                (None, _) if selected => {
                    cell.set_char(' ').set_bg(shade);
                }
                _ => {}
            }
        }
    }
}

/// Draw rows `top..` of a list so that `cursor` is on screen, moving `top` as
/// little as possible. Only the rows that fit are built. Returns each drawn
/// item with the character offset its visual rows start at.
#[allow(clippy::too_many_arguments)]
fn render_window(
    theme: &Theme,
    f: &mut Frame,
    area: Rect,
    block: Block,
    len: usize,
    cursor: usize,
    top: &mut usize,
    mut item: impl FnMut(usize) -> (Vec<Line<'static>>, Vec<usize>),
) -> Vec<(usize, Vec<usize>)> {
    let height = block.inner(area).height as usize;
    if len == 0 || height == 0 {
        f.render_widget(block, area);
        return Vec::new();
    }
    let cursor = cursor.min(len - 1);
    *top = (*top).min(cursor).max(cursor.saturating_sub(height - 1));

    let mut rows: VecDeque<(usize, Vec<Line<'static>>, Vec<usize>)> = VecDeque::new();
    let mut used = 0;
    for p in *top..=cursor {
        let (lines, firsts) = item(p);
        used += lines.len();
        rows.push_back((p, lines, firsts));
    }
    while used > height && rows.len() > 1 {
        used -= rows.pop_front().unwrap().1.len();
    }
    *top = rows.front().unwrap().0;
    let mut next = cursor + 1;
    while used < height && next < len {
        let (lines, firsts) = item(next);
        used += lines.len();
        rows.push_back((next, lines, firsts));
        next += 1;
    }

    let drawn = rows.iter().map(|(p, _, firsts)| (*p, firsts.clone())).collect();
    let items: Vec<ListItem> = rows
        .into_iter()
        .map(|(p, lines, _)| {
            let item = ListItem::new(Text::from(lines));
            if p == cursor { item.style(cursor_style(theme)) } else { item }
        })
        .collect();
    f.render_widget(List::new(items).block(block), area);
    drawn
}

fn draw_patterns(f: &mut Frame, app: &mut App, area: Rect) {
    let theme = theme_of(app);
    let title = format!(
        " Patterns  {} unique / {} lines ",
        app.patterns.len(),
        app.total_matching()
    );
    let block = Block::bordered().title(title.bold());
    if app.patterns.is_empty() {
        f.render_widget(
            Paragraph::new("Nothing matches the current filters.")
                .style(Style::new().fg(theme.dim))
                .block(block),
            area,
        );
        return;
    }

    // The biggest count is first. Its width, two spaces, the level, then the rule.
    let count_w = app.patterns[0].count.to_string().len().max(7);
    let prefix = count_w + 2 + 5 + DIVIDER.chars().count();
    let budget = (block.inner(area).width as usize).saturating_sub(prefix);
    let wrap = app.wrap;
    let wide = (!wrap && budget > 0 && app.max_pattern > budget).then_some((app.max_pattern, app.hscroll, budget));
    let block = if wide.is_some() { block.padding(Padding::bottom(1)) } else { block };
    let inner = block.inner(area);
    app.page_rows = inner.height as usize;
    let find = app.find.as_ref().map(|q| &q.re);
    let sel = app.live_selection();
    let kind = app.kind;
    let hscroll = if wrap { 0 } else { app.hscroll };
    let cursor = app.selected;
    let max_rows = wrap_rows(inner);
    let cont = vec![Span::raw(" ".repeat(count_w + 2 + 5)), divider(theme)];
    let mut top = app.tops[View::Patterns as usize];
    let drawn = render_window(theme, f, area, block, app.patterns.len(), cursor, &mut top, |i| {
        let p = &app.patterns[i];
        let mut spans = vec![
            Span::styled(format!("{:>count_w$}  ", p.count), Style::default().fg(theme.bright).bold()),
            Span::styled(format!("{:<5}", p.severity.label_for(kind)), Style::default().fg(theme.severity(p.severity))),
            divider(theme),
        ];
        let select = sel.filter(|s| s.pos == i).map(|s| s.range());
        if wrap && i == cursor {
            let paint = |at: usize, seg: &str, s| {
                let masks: Vec<_> = p
                    .masks
                    .iter()
                    .filter(|r| r.start >= at && r.end <= at + seg.len())
                    .map(|r| r.start - at..r.end - at)
                    .collect();
                highlight::pattern(seg, &masks, find, s, kind, theme)
            };
            return wrapped_lines(&p.text, budget, max_rows, paint, select, &spans, &cont, theme.dim);
        }
        let body = fold_octal_nl(&highlight::pattern(&p.text, &p.masks, find, select, kind, theme));
        spans.extend(clip(&body, hscroll, budget, theme.dim));
        (vec![Line::from(spans)], vec![hscroll])
    });
    app.tops[View::Patterns as usize] = top;
    app.rows = row_hits(inner, &drawn, inner.x + prefix as u16);
    let (len, shown) = (app.patterns.len(), drawn.len());
    paint_scrollbars(f, app, area, inner, len, top, shown, wide);
}

fn draw_lines(f: &mut Frame, app: &mut App, area: Rect) {
    let theme = theme_of(app);
    let all = app.view == View::All;
    let list: &[usize] = if all { &app.visible } else { app.selected_lines() };
    let block = if all {
        Block::bordered()
            .title(format!(" All lines  {} of {} ", list.len(), app.entries.len()).bold())
    } else {
        let pattern = app.selected_pattern().map(|p| p.text.clone()).unwrap_or_default();
        Block::bordered()
            .title(format!(" {} raw lines ", list.len()).bold())
            .title_bottom(truncate(&pattern, area.width.saturating_sub(4) as usize).fg(theme.dim))
    };

    let include = app.include.as_ref().map(|q| &q.re);
    let find = app.find.as_ref().map(|q| &q.re);
    let sel = app.live_selection();
    let (wrap, hscroll) = (app.wrap, app.hscroll);
    let kind = app.kind;
    let cursor = if all { app.all_cursor } else { app.line_cursor };
    let mut top = app.tops[app.view as usize];
    let entries = &app.entries;
    // Lists are in file order, so the last line number is the widest.
    let digits = list.last().map_or(0, |&i| entries[i].line_no.to_string().len()).max(7);
    let files = &app.files;
    let name_w = files.iter().map(|s| s.chars().count()).max().unwrap_or(0).min(MAX_FILE_NAME);
    let indent = digits + name_w + 2 * DIVIDER.chars().count();
    let budget = (block.inner(area).width as usize).saturating_sub(indent);
    let wide = (!wrap && budget > 0 && app.max_raw > budget).then_some((app.max_raw, hscroll, budget));
    let block = if wide.is_some() { block.padding(Padding::bottom(1)) } else { block };
    let inner = block.inner(area);
    let cont = vec![Span::raw(" ".repeat(digits)), divider(theme), Span::raw(" ".repeat(name_w)), divider(theme)];
    let hscroll = if wrap { 0 } else { hscroll };
    let max_rows = wrap_rows(inner);
    let n = list.len();

    let drawn = render_window(theme, f, area, block, n, cursor, &mut top, |i| {
        let e = &entries[list[i]];
        let name = files.get(e.file).map_or("", String::as_str);
        let gutter = vec![
            Span::styled(format!("{:>digits$}", e.line_no), Style::default().fg(theme.severity(e.severity))),
            divider(theme),
            Span::styled(format!("{:<name_w$}", truncate(name, name_w)), Style::default().fg(theme.purple)),
            divider(theme),
        ];
        let select = sel.filter(|s| s.pos == i).map(|s| s.range());
        if wrap && i == cursor {
            let paint = |_, seg: &str, s| highlight::raw(seg, include, find, s, kind, theme);
            wrapped_lines(e.raw.trim_end(), budget, max_rows, paint, select, &gutter, &cont, theme.dim)
        } else {
            let body = fold_octal_nl(&highlight::raw(e.raw.trim_end(), include, find, select, kind, theme));
            let mut spans = gutter;
            spans.extend(clip(&body, hscroll, budget, theme.dim));
            (vec![Line::from(spans)], vec![hscroll])
        }
    });
    let shown = drawn.len();
    app.page_rows = inner.height as usize;
    app.tops[app.view as usize] = top;
    app.rows = row_hits(inner, &drawn, inner.x + indent as u16);
    paint_scrollbars(f, app, area, inner, n, top, shown, wide);
}

fn draw_status(f: &mut Frame, app: &mut App, area: Rect) {
    let theme = theme_of(app);
    if app.confirm_quit {
        let spans = vec![
            Span::styled(" Quit ", Style::default().fg(ink(theme.error_lit)).bg(theme.error_lit)),
            Span::styled(" Are you sure? (y/n)", Style::default().fg(theme.bright).bold()),
        ];
        f.render_widget(Paragraph::new(Line::from(spans)), area);
        return;
    }
    if let Some(p) = &app.prompt {
        let label = format!(" {}: ", p.kind.label());
        let err = app.message.as_ref().map(|m| format!("   {m}")).unwrap_or_default();
        let room = (area.width as usize)
            .saturating_sub(label.chars().count() + err.chars().count())
            .max(1);
        let mut spans = vec![Span::styled(label, Style::default().fg(ink(theme.accent)).bg(theme.accent))];
        spans.extend(prompt_line(&p.input, room, theme));
        if let Some(m) = &app.message {
            spans.push(Span::styled(format!("   {m}"), Style::default().fg(theme.error_lit)));
        }
        f.render_widget(Paragraph::new(Line::from(spans)), area);
        return;
    }

    let mut spans = vec![
        Span::styled(format!(" {} ", app.source), Style::default().fg(ink(theme.accent)).bg(theme.accent)),
        Span::raw(" "),
        if app.case_sensitive {
            Span::styled("Aa", Style::default().fg(theme.bright).bold())
        } else {
            Span::styled("Aa", Style::default().fg(theme.dim))
        },
        Span::raw(" "),
    ];
    for &(letter, s) in level_marks(app.kind) {
        spans.push(if app.levels[s as usize] {
            Span::styled(letter, Style::default().fg(theme.severity(s)).bold())
        } else {
            Span::styled(letter, Style::default().fg(theme.inactive))
        });
    }
    if let Some(q) = &app.include {
        spans.push(Span::styled(format!("  +{}", truncate(&q.src, 24)), Style::default().fg(theme.bright).underlined()));
    }
    if let Some(q) = &app.exclude {
        spans.push(Span::styled(format!("  -{}", truncate(&q.src, 24)), Style::default().fg(theme.bright).crossed_out()));
    }
    if let Some(t) = app.time {
        spans.push(Span::styled(format!("  @{}", parse::format_range(t)), Style::default().fg(theme.bright)));
    }
    let find = app.find.as_ref().map(|q| truncate(&q.src, 16));
    if let (Some(term), Some((at, n))) = (find, app.find_status()) {
        let at = at.map_or("-".to_string(), |a| (a + 1).to_string());
        spans.push(Span::styled(format!("  /{term} {at}/{n}"), Style::default().fg(theme.find_bg)));
    }
    let len = app.list_len();
    let pos = if len == 0 { 0 } else { app.cursor() + 1 };
    spans.push(Span::styled(format!("  {pos}/{len}"), Style::default().fg(theme.bright)));
    let mut flags = String::new();
    for (on, name) in [(app.wrap, " wrap"), (app.power_scroll, " power"), (!app.mouse, " nomouse")] {
        if on {
            flags.push_str(name);
        }
    }
    spans.push(Span::styled(flags, Style::default().fg(theme.purple)));
    match &app.message {
        Some(m) => spans.push(Span::styled(format!("   {m}"), Style::default().fg(theme.error_lit))),
        None => spans.push(Span::styled("   ? help", Style::default().fg(theme.dim))),
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// The part of a prompt that fits `room` cells, with the cursor drawn in it.
/// One cell per character, same as the rest of the lists.
fn prompt_line(input: &tui_input::Input, room: usize, theme: &Theme) -> Vec<Span<'static>> {
    let chars: Vec<char> = input.value().chars().collect();
    let cursor = input.cursor().min(chars.len());
    let at_end = cursor == chars.len();
    let start = if at_end {
        cursor.saturating_sub(room.saturating_sub(1))
    } else {
        (cursor + 1).saturating_sub(room)
    };
    let before: String = chars[start..cursor].iter().collect();
    let mut spans = vec![Span::raw(before)];
    if at_end {
        spans.push(Span::styled("\u{258f}", Style::default().fg(theme.bright)));
    } else {
        let cursor_style = Style::default().fg(ink(theme.bright)).bg(theme.bright);
        spans.push(Span::styled(chars[cursor].to_string(), cursor_style));
        let after_budget = room.saturating_sub(chars[start..=cursor].len());
        let after: String = chars[cursor + 1..].iter().take(after_budget).collect();
        if !after.is_empty() {
            spans.push(Span::raw(after));
        }
    }
    spans
}

const HELP: &[(&str, &str)] = &[
    ("\u{2191}\u{2193} j k  PgUp PgDn  Home End", "move; mouse wheel scrolls"),
    ("\u{2190} \u{2192}", "scroll long lines sideways"),
    ("Enter", "pattern \u{2192} its lines \u{2192} line in context \u{2192} its pattern"),
    ("a", "all filtered lines, in file order"),
    ("Esc", "clear selection, then find, then go back; on patterns, Files, Settings, Themes or Quit"),
    ("Backspace", "go back"),
    ("e w i d o", "toggle Error Warn Info Debug Other; on a web log, 5xx 4xx 2xx 1xx other"),
    ("E W I D O", "show only that level; again for all"),
    ("5 4 3 2 1 0", "web log only: same toggles (3 and 2 are both 2xx/3xx; 0 is other)"),
    ("Space / Tab", "include / exclude regex (empty clears)"),
    ("in a prompt", "arrows move; Home End; Ctrl-arrows by word; Ctrl-Backspace / Ctrl-Delete delete a word"),
    ("c", "case sensitivity for every regex"),
    ("t", "time range: 15:55  15:55..15:58  15:55..+5m  Feb 15  (empty clears)"),
    ("click / drag on timeline", "jump to that time; drag filters to those buckets"),
    ("right-click timeline", "clear the time filter, or choose a period"),
    ("double-click the gutter", "same as Enter"),
    ("drag / 2-click / 3-click", "select text; a word; up to whitespace"),
    ("right click", "include, exclude or find the selection"),
    ("f   n N", "find regex; next / previous match"),
    ("g", "go to line number"),
    ("l  p  s", "wrap the selected line (#012 breaks; otherwise it shows as \u{23ce}); power scroll; mouse off"),
    ("%IP% %UUID% %FIND%", "macros, expand inside any regex"),
    ("q", "quit, after asking; Ctrl-C quits at once"),
];

fn draw_help(f: &mut Frame, theme: &Theme) {
    let key_w = HELP.iter().map(|(k, _)| k.chars().count()).max().unwrap_or(0);
    let lines: Vec<Line> = HELP
        .iter()
        .map(|(k, d)| {
            Line::from(vec![
                Span::styled(format!(" {k:<key_w$}  "), Style::default().fg(theme.accent).bold()),
                Span::raw(*d),
            ])
        })
        .collect();
    let width = lines.iter().map(|l| l.width()).max().unwrap_or(0) as u16 + 3;
    let [area] = Layout::horizontal([Constraint::Length(width)]).flex(Flex::Center).areas(f.area());
    let [area] = Layout::vertical([Constraint::Length(lines.len() as u16 + 2)])
        .flex(Flex::Center)
        .areas(area);
    f.render_widget(Clear, area);
    fill_bg(f, area, theme);
    let mut block = Block::bordered()
        .title(" Sawmill — Dissecting Logs ".bold())
        .title_bottom(" any key closes ".fg(theme.dim));
    if theme.bg != Color::Reset {
        block = block.style(Style::new().bg(theme.bg).fg(theme.text));
    }
    f.render_widget(
        Paragraph::new(lines).block(block),
        area,
    );
}

/// `#012` drawn as a return arrow. A wrapped row breaks there instead.
fn fold_octal_nl(spans: &[Span<'static>]) -> Vec<Span<'static>> {
    let mut chars: Vec<(char, Style)> = Vec::new();
    for s in spans {
        for c in s.content.chars() {
            chars.push((c, s.style));
        }
    }
    let mut out: Vec<Span<'static>> = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let (c, style) = chars[i];
        let octal = c == '#'
            && chars.get(i + 1).is_some_and(|x| x.0 == '0')
            && chars.get(i + 2).is_some_and(|x| x.0 == '1')
            && chars.get(i + 3).is_some_and(|x| x.0 == '2');
        if octal {
            push_styled(&mut out, '\u{23ce}', style);
            i += 4;
        } else {
            push_styled(&mut out, c, style);
            i += 1;
        }
    }
    out
}

fn push_styled(out: &mut Vec<Span<'static>>, c: char, style: Style) {
    if let Some(last) = out.last_mut()
        && last.style == style
    {
        last.content.to_mut().push(c);
    } else {
        out.push(Span::styled(c.to_string(), style));
    }
}

/// Columns `skip..skip + width` of a styled line, with `…` where it is cut.
fn clip(spans: &[Span<'static>], skip: usize, width: usize, dim_colour: Color) -> Vec<Span<'static>> {
    let dim = Style::default().fg(dim_colour);
    if width == 0 {
        return Vec::new();
    }
    let total: usize = spans.iter().map(|s| s.content.chars().count()).sum();
    let mut out = Vec::new();
    let mut room = width;
    let mut skip = skip.min(total);
    if skip > 0 {
        out.push(Span::styled("\u{2026}", dim));
        room -= 1;
        skip += 1;
    }
    let cut = total > skip + room;
    if cut {
        room = room.saturating_sub(1);
    }
    let mut pos = 0;
    for s in spans {
        let n = s.content.chars().count();
        let (lo, hi) = (skip.max(pos), (skip + room).min(pos + n));
        if lo < hi {
            let text: String = s.content.chars().skip(lo - pos).take(hi - lo).collect();
            out.push(Span::styled(text, s.style));
        }
        pos += n;
    }
    if cut {
        out.push(Span::styled("\u{2026}", dim));
    }
    out
}

/// Pieces of `text` split on `#012`, each with its character offset in `text`.
fn split_octal_nl(text: &str) -> Vec<(usize, &str)> {
    let mut out = Vec::new();
    let mut chars = 0;
    let mut rest = text;
    while let Some(i) = rest.find(OCTAL_NL) {
        let seg = &rest[..i];
        out.push((chars, seg));
        chars += seg.chars().count() + OCTAL_NL.chars().count();
        rest = &rest[i + OCTAL_NL.len()..];
    }
    out.push((chars, rest));
    out
}

fn local_select(select: &std::ops::Range<usize>, origin: usize, len: usize) -> Option<std::ops::Range<usize>> {
    let lo = select.start.max(origin);
    let hi = select.end.min(origin.saturating_add(len));
    // `then_some` evaluates its argument even when the condition is false, and
    // `hi - origin` overflows when the selection ends before this segment.
    if lo < hi {
        Some((lo - origin)..(hi - origin))
    } else {
        None
    }
}

fn push_lead(line: &mut Line<'static>, lead: &[Span<'static>]) {
    for (i, span) in lead.iter().enumerate() {
        line.spans.insert(i, span.clone());
    }
}

/// A wrapped line may take up to three quarters of the list, so the rows
/// around it stay in sight.
fn wrap_rows(inner: Rect) -> usize {
    (inner.height as usize * 3 / 4).max(1)
}

/// Wrap `raw` into at most `max_rows` rows, treating `#012` as a newline and
/// ending with `…` if cut short. `paint` highlights one piece, given its byte
/// offset in `raw` and its share of the selection. `firsts` are character
/// offsets into `raw`.
#[allow(clippy::too_many_arguments)]
fn wrapped_lines(
    raw: &str,
    budget: usize,
    max_rows: usize,
    paint: impl Fn(usize, &str, Option<std::ops::Range<usize>>) -> Vec<Span<'static>>,
    select: Option<std::ops::Range<usize>>,
    gutter: &[Span<'static>],
    cont: &[Span<'static>],
    dim: Color,
) -> (Vec<Line<'static>>, Vec<usize>) {
    let budget = budget.max(1);
    let max_rows = max_rows.max(1);
    let mut lines = Vec::new();
    let mut firsts = Vec::new();
    for (origin, seg) in split_octal_nl(raw) {
        let at = seg.as_ptr() as usize - raw.as_ptr() as usize;
        let local = select.as_ref().and_then(|r| local_select(r, origin, seg.chars().count()));
        let body = paint(at, seg, local);
        for (k, mut line) in wrap_spans(body, budget).into_iter().enumerate() {
            if lines.len() == max_rows {
                mark_cut(lines.last_mut().unwrap(), budget, cont.iter().map(|s| s.width()).sum(), dim);
                return (lines, firsts);
            }
            push_lead(&mut line, if lines.is_empty() { gutter } else { cont });
            lines.push(line);
            firsts.push(origin + k * budget);
        }
    }
    (lines, firsts)
}

/// End a row with `…`, giving up its last character if the row is full.
fn mark_cut(line: &mut Line<'static>, budget: usize, lead: usize, dim: Color) {
    if line.width() >= lead + budget
        && let Some(span) = line.spans.iter_mut().rev().find(|s| !s.content.is_empty())
    {
        let mut text = span.content.to_string();
        text.pop();
        span.content = text.into();
    }
    line.spans.push(Span::styled("\u{2026}", Style::default().fg(dim)));
}

/// Break a styled line into rows of at most `width` characters.
fn wrap_spans(spans: Vec<Span<'static>>, width: usize) -> Vec<Line<'static>> {
    let width = width.max(1);
    let mut lines = Vec::new();
    let mut row: Vec<Span<'static>> = Vec::new();
    let mut used = 0;
    for s in spans {
        let mut buf = String::new();
        for ch in s.content.chars() {
            if used == width {
                if !buf.is_empty() {
                    row.push(Span::styled(std::mem::take(&mut buf), s.style));
                }
                lines.push(Line::from(std::mem::take(&mut row)));
                used = 0;
            }
            buf.push(ch);
            used += 1;
        }
        if !buf.is_empty() {
            row.push(Span::styled(buf, s.style));
        }
    }
    if !row.is_empty() || lines.is_empty() {
        lines.push(Line::from(row));
    }
    lines
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

    fn text(spans: &[Span]) -> String {
        spans.iter().map(|s| s.content.as_ref()).collect()
    }

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

    #[test]
    fn clip_fits_and_marks_both_ends() {
        let spans = vec![Span::raw("abc"), Span::raw("defgh")];
        let dim = Color::DarkGray;
        assert_eq!(text(&clip(&spans, 0, 8, dim)), "abcdefgh");
        assert_eq!(text(&clip(&spans, 0, 5, dim)), "abcd\u{2026}");
        assert_eq!(text(&clip(&spans, 2, 4, dim)), "\u{2026}de\u{2026}");
        assert_eq!(text(&clip(&spans, 5, 10, dim)), "\u{2026}gh");
    }

    #[test]
    fn wrap_splits_across_spans() {
        let lines = wrap_spans(vec![Span::raw("abc"), Span::raw("defg")], 3);
        let rows: Vec<String> = lines.iter().map(|l| text(&l.spans)).collect();
        assert_eq!(rows, ["abc", "def", "g"]);
    }

    #[test]
    fn an_unwrapped_line_draws_octal_newline_as_a_return() {
        let spans = fold_octal_nl(&[Span::styled("ab#012c#012", Style::default().fg(Color::Green))]);
        assert_eq!(text(&spans), "ab\u{23ce}c\u{23ce}");
        assert_eq!(spans[0].style.fg, Some(Color::Green));
        let split = fold_octal_nl(&[Span::raw("a#"), Span::styled("012b", Style::default().fg(Color::Red))]);
        assert_eq!(text(&split), "a\u{23ce}b");
        assert_eq!(split[0].content.as_ref(), "a\u{23ce}");
        assert_eq!(split[0].style.fg, None, "the arrow keeps the style of the #");
        assert_eq!(split[1].style.fg, Some(Color::Red));
    }

    #[test]
    fn a_selection_before_a_break_does_not_overflow() {
        let (lines, _) = wrapped_lines(
            "ab#012cdef",
            10,
            10,
            |_, seg, s| highlight::raw(seg, None, None, s, LogKind::System, &theme::TERMINAL),
            Some(0..2),
            &[Span::raw("")],
            &[Span::raw("")],
            Color::DarkGray,
        );
        assert_eq!(lines.len(), 2);
    }

    #[test]
    fn octal_012_splits_a_wrapped_line_and_keeps_offsets() {
        assert_eq!(split_octal_nl("ab#012c#012#012d"), vec![(0, "ab"), (6, "c"), (11, ""), (15, "d")]);
        let (lines, firsts) = wrapped_lines(
            "ab#012cdef",
            3,
            10,
            |_, seg, s| highlight::raw(seg, None, None, s, LogKind::System, &theme::TERMINAL),
            None,
            &[Span::raw("G ")],
            &[Span::raw("  ")],
            Color::DarkGray,
        );
        let rows: Vec<String> = lines.iter().map(|l| text(&l.spans)).collect();
        assert_eq!(rows, ["G ab", "  cde", "  f"]);
        assert_eq!(firsts, [0, 6, 9]);
    }

    #[test]
    fn a_wrapped_line_stops_at_its_row_limit_with_an_ellipsis() {
        let paint = |_, seg: &str, s| highlight::raw(seg, None, None, s, LogKind::System, &theme::TERMINAL);
        let (lines, firsts) = wrapped_lines("abcdefghij", 3, 2, paint, None, &[Span::raw("G ")], &[Span::raw("  ")], Color::DarkGray);
        let rows: Vec<String> = lines.iter().map(|l| text(&l.spans)).collect();
        assert_eq!(rows, ["G abc", "  de\u{2026}"]);
        assert_eq!(firsts, [0, 3]);
    }

    #[test]
    fn wrap_rows_is_three_quarters_of_the_list() {
        assert_eq!(wrap_rows(Rect::new(0, 0, 10, 20)), 15);
        assert_eq!(wrap_rows(Rect::new(0, 0, 10, 1)), 1);
    }

    fn bars(counts: [usize; 5], height: u16) -> Buffer {
        let area = Rect::new(0, 0, 1, height);
        let mut buf = Buffer::empty(area);
        render_bars(&[Bucket { start: chrono::NaiveDateTime::MIN, counts }], area, &mut buf, &theme::TERMINAL, |_| false);
        buf
    }

    #[test]
    fn a_lit_column_is_lighter_on_grey() {
        let mut counts = [0; 5];
        counts[Severity::Info as usize] = 1;
        counts[Severity::Error as usize] = 1;
        let tall = Bucket { start: chrono::NaiveDateTime::MIN, counts: [0, 0, 6, 0, 0] };
        let area = Rect::new(0, 0, 2, 3);
        let mut buf = Buffer::empty(area);
        render_bars(&[Bucket { start: tall.start, counts }, tall], area, &mut buf, &theme::TERMINAL, |i| i == 0);
        let bottom = &buf[(0, 2)];
        assert_eq!((bottom.fg, bottom.bg), (Color::LightCyan, Color::LightRed));
        assert_eq!(buf[(0, 0)].bg, Color::Indexed(237), "the empty part is grey");
        assert_eq!(buf[(1, 2)].fg, Color::Cyan, "other columns are untouched");
    }

    #[test]
    fn bars_have_half_cell_resolution() {
        // Three units of Info on a two-row column: one full cell, then a lower half.
        let mut counts = [0; 5];
        counts[Severity::Info as usize] = 3;
        let mut tall = counts;
        tall[Severity::Info as usize] = 4;
        let peak = Bucket { start: chrono::NaiveDateTime::MIN, counts: tall };
        let area = Rect::new(0, 0, 2, 2);
        let mut buf = Buffer::empty(area);
        render_bars(&[Bucket { start: peak.start, counts }, peak], area, &mut buf, &theme::TERMINAL, |_| false);
        let bottom = &buf[(0, 1)];
        let top = &buf[(0, 0)];
        assert_eq!((bottom.symbol(), bottom.fg, bottom.bg), ("\u{2584}", Color::Cyan, Color::Cyan));
        assert_eq!((top.symbol(), top.fg, top.bg), ("\u{2584}", Color::Cyan, Color::Reset));
    }

    #[test]
    fn bars_stack_the_worst_level_on_top() {
        let mut counts = [0; 5];
        counts[Severity::Info as usize] = 3;
        counts[Severity::Error as usize] = 1;
        let buf = bars(counts, 2);
        let top = &buf[(0, 0)];
        assert_eq!((top.fg, top.bg), (Color::Cyan, Color::Red));
        let bottom = &buf[(0, 1)];
        assert_eq!((bottom.fg, bottom.bg), (Color::Cyan, Color::Cyan));
    }
}
