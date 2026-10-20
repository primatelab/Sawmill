use crate::config;
use crate::parse::{self, Entry, FileLog, LogKind, Severity};
use crate::theme;
use chrono::{NaiveDate, NaiveDateTime, TimeDelta};
use regex::{NoExpand, Regex, RegexBuilder};
use std::collections::HashMap;
use std::path::PathBuf;

/// Bucket widths the histogram is allowed to snap to, smallest first.
const LADDER_SECS: [i64; 12] = [
    1, 5, 15, 30, 60, 300, 900, 1800, 3600, 21600, 43200, 86400,
];

/// The severity a group is mostly made of. Labelling by the single most severe
/// member lets one stray line paint hundreds of benign ones red.
/// Ties go to the more severe: `max_by_key` keeps the last maximum, so the list
/// is walked least-severe-first.
fn dominant(counts: &[usize; 5]) -> Severity {
    Severity::ALL
        .iter()
        .rev()
        .copied()
        .max_by_key(|s| counts[*s as usize])
        .unwrap_or(Severity::Other)
}

/// Split a bar of `units` cells between severities in proportion to their counts.
/// A severity that is present keeps at least one cell, taken from the biggest
/// share, so one error in a busy bucket still shows.
pub fn allocate(counts: &[usize; 5], units: usize) -> [usize; 5] {
    let mut out = [0; 5];
    let total: usize = counts.iter().sum();
    if total == 0 || units == 0 {
        return out;
    }
    let mut given = 0;
    let mut remainders: Vec<(usize, usize)> = Vec::with_capacity(5);
    for (i, &c) in counts.iter().enumerate() {
        let share = c * units;
        out[i] = share / total;
        given += share / total;
        remainders.push((share % total, i));
    }
    remainders.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    for &(_, i) in remainders.iter().take(units - given) {
        out[i] += 1;
    }
    for s in Severity::ALL {
        let i = s as usize;
        if counts[i] == 0 || out[i] > 0 {
            continue;
        }
        let Some(donor) = (0..5).filter(|&d| out[d] > 1).max_by_key(|&d| (out[d], d)) else {
            break;
        };
        out[donor] -= 1;
        out[i] = 1;
    }
    out
}

/// Indices in `0..n` that pass `keep`, in order, checked across all cores.
fn par_filter(n: usize, keep: impl Fn(usize) -> bool + Sync) -> Vec<usize> {
    let threads = std::thread::available_parallelism().map_or(1, |t| t.get());
    let chunk = n.div_ceil(threads).max(4096);
    std::thread::scope(|s| {
        let keep = &keep;
        let parts: Vec<_> = (0..n)
            .step_by(chunk)
            .map(|lo| s.spawn(move || (lo..(lo + chunk).min(n)).filter(|&i| keep(i)).collect::<Vec<_>>()))
            .collect();
        parts.into_iter().flat_map(|p| p.join().unwrap()).collect()
    })
}

/// Position of `entry` in an ascending list of entry indices, or of the next one after it.
fn nearest(list: &[usize], entry: usize) -> usize {
    list.partition_point(|&e| e < entry).min(list.len().saturating_sub(1))
}

pub struct Pattern {
    /// `Entry::pattern` of its lines.
    pub key: String,
    /// The key as shown, with mask names in place of placeholders.
    pub text: String,
    /// Byte ranges of the mask names in `text`.
    pub masks: Vec<std::ops::Range<usize>>,
    pub count: usize,
    pub severity: Severity,
    /// Indices into `App::entries`.
    pub lines: Vec<usize>,
}

#[derive(Clone)]
pub struct Bucket {
    pub start: NaiveDateTime,
    pub counts: [usize; 5],
}

impl Bucket {
    pub fn total(&self) -> usize {
        self.counts.iter().sum()
    }
}

#[derive(PartialEq, Eq, Clone, Copy, Debug)]
pub enum View {
    Patterns,
    Lines,
    All,
}

#[derive(PartialEq, Eq, Clone, Copy, Debug)]
pub enum PromptKind {
    Include,
    Exclude,
    Find,
    Time,
    GoTo,
    AddFiles,
}

impl PromptKind {
    pub fn label(self) -> &'static str {
        match self {
            PromptKind::AddFiles => "Add files (globs, space separated)",
            PromptKind::Include => "Include",
            PromptKind::Exclude => "Exclude",
            PromptKind::Find => "Find",
            PromptKind::Time => "Time",
            PromptKind::GoTo => "Go to line",
        }
    }
}

pub struct Prompt {
    pub kind: PromptKind,
    pub input: tui_input::Input,
}

/// A typed regex, kept as typed so a case change can rebuild it.
pub struct Query {
    pub src: String,
    pub re: Regex,
}

/// A rectangle in screen cells. The timeline's outer frame, including its border.
#[derive(Clone, Copy, Debug)]
pub struct ScreenBox {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

impl ScreenBox {
    pub fn contains(self, col: u16, row: u16) -> bool {
        col >= self.x
            && row >= self.y
            && col < self.x.saturating_add(self.width)
            && row < self.y.saturating_add(self.height)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Axis {
    Vertical,
    Horizontal,
}

/// Where a scrollbar was drawn: `cells` long from `x`, `y`, arrows included.
/// Positions run `0..=max`.
#[derive(Clone, Copy, Debug)]
pub struct Bar {
    pub axis: Axis,
    pub x: u16,
    pub y: u16,
    pub cells: u16,
    pub max: usize,
}

impl Bar {
    pub fn contains(&self, col: u16, row: u16) -> bool {
        match self.axis {
            Axis::Vertical => col == self.x && row >= self.y && row < self.y + self.cells,
            Axis::Horizontal => row == self.y && col >= self.x && col < self.x + self.cells,
        }
    }

    fn along(&self, col: u16, row: u16) -> u16 {
        match self.axis {
            Axis::Vertical => row.saturating_sub(self.y),
            Axis::Horizontal => col.saturating_sub(self.x),
        }
    }

    /// The position a cell on the track stands for, scaled from the ends of the
    /// track between the arrows.
    pub fn position_at(&self, col: u16, row: u16) -> usize {
        let track = self.cells.saturating_sub(2).max(1) as usize;
        let at = (self.along(col, row) as usize).saturating_sub(1).min(track - 1);
        if track == 1 {
            return 0;
        }
        (at * self.max + (track - 1) / 2) / (track - 1)
    }
}

/// Where the timeline bars were last drawn, so mouse columns map to buckets.
#[derive(Clone, Copy)]
pub struct HistGeom {
    pub x: u16,
    pub y: u16,
    pub height: u16,
    pub len: usize,
    pub start: NaiveDateTime,
    pub secs: i64,
}

impl HistGeom {
    pub fn contains(&self, col: u16, row: u16) -> bool {
        col >= self.x && ((col - self.x) as usize) < self.len && row >= self.y && row < self.y + self.height
    }

    /// The bucket under a column, clamped to the bars.
    pub fn bucket_at(&self, col: u16) -> usize {
        (col.saturating_sub(self.x) as usize).min(self.len.saturating_sub(1))
    }
}

/// Where one screen row of the main list was drawn. Columns before `x` are the
/// count, level or line number; `x` is the first column of the text, which shows
/// character `first` of list item `pos`.
#[derive(Clone, Copy, Debug)]
pub struct RowHit {
    pub y: u16,
    pub x: u16,
    pub pos: usize,
    pub first: usize,
}

/// Characters picked with the mouse inside one list item, `anchor` and `head` inclusive.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Selection {
    key: SelKey,
    pub pos: usize,
    pub anchor: usize,
    pub head: usize,
}

impl Selection {
    pub fn range(&self) -> std::ops::Range<usize> {
        self.anchor.min(self.head)..self.anchor.max(self.head) + 1
    }
}

/// A selection is only good for the list it was made on: view, selected pattern, filter generation.
type SelKey = (View, usize, u64);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MenuAction {
    Include,
    Exclude,
    Find,
    ClearTime,
    ChooseTime,
    AddFiles,
    /// Index into `App::files`.
    RemoveFile(usize),
    ClearFilters,
    OpenSettings,
    OpenFiles,
    ToggleWrap,
    ToggleMouse,
    ToggleCase,
    TogglePower,
    EditInclude,
    EditExclude,
    OpenThemes,
    /// Index into `theme::NAMES`.
    SetTheme(u8),
    Quit,
    /// Shown in a menu and never chosen. The files menu uses this for stdin.
    Inert,
}

/// A popup menu. `x`, `y` is its top-left corner, already placed on screen.
pub struct Menu {
    pub x: u16,
    pub y: u16,
    pub title: Option<&'static str>,
    /// The selection the Include, Exclude and Find items act on.
    pub text: String,
    pub entries: Vec<(String, MenuAction)>,
    pub at: usize,
}

impl Menu {
    pub fn items(&self) -> Vec<&str> {
        self.entries.iter().map(|(s, _)| s.as_str()).collect()
    }

    pub fn width(&self) -> u16 {
        let title = self.title.map_or(0, |t| t.chars().count() + 2);
        self.items().iter().map(|s| s.chars().count()).max().unwrap_or(0).max(title) as u16 + 4
    }

    pub fn height(&self) -> u16 {
        self.items().len() as u16 + 2
    }

    /// An item the cursor can land on. `Inert` rows are drawn and skipped.
    pub fn selectable(&self, i: usize) -> bool {
        self.entries.get(i).is_some_and(|(_, a)| !matches!(a, MenuAction::Inert))
    }

    /// Move the highlight by one selectable row. Inert rows are stepped over,
    /// and the highlight stays put at either end.
    pub fn move_at(&mut self, down: bool) {
        let step = if down { 1 } else { -1 };
        let mut i = self.at as isize;
        loop {
            i += step;
            if !(0..self.entries.len() as isize).contains(&i) {
                return;
            }
            if self.selectable(i as usize) {
                self.at = i as usize;
                return;
            }
        }
    }

    /// The item under a screen cell, if the cell is on one.
    pub fn item_at(&self, col: u16, row: u16) -> Option<usize> {
        let inside = col > self.x && col < self.x + self.width() - 1;
        let on_row = row > self.y && row < self.y + self.height() - 1;
        on_row.then(|| (row - self.y - 1) as usize).filter(|_| inside).filter(|&i| i < self.items().len())
    }

    /// The item whose label starts with `c`, when exactly one does.
    pub fn key_item(&self, c: char) -> Option<usize> {
        let mut hit = None;
        for (i, s) in self.items().iter().enumerate() {
            if !self.selectable(i) {
                continue;
            }
            if s.starts_with(c.to_ascii_uppercase()) {
                if hit.is_some() {
                    return None;
                }
                hit = Some(i);
            }
        }
        hit
    }

    fn place(col: u16, row: u16, screen: (u16, u16), w: u16, h: u16) -> (u16, u16) {
        let x = col.min(screen.0.saturating_sub(w));
        let y = if row.saturating_add(h) < screen.1 { row + 1 } else { row.saturating_sub(h) };
        (x, y)
    }
}

/// Characters that make up a word: a double click takes alphanumerics, `-`
/// and `_`; a triple click takes everything up to whitespace.
fn word_char(c: char, big: bool) -> bool {
    if big {
        !matches!(c, ' ' | '\t' | '\n' | '\r')
    } else {
        c.is_alphanumeric() || c == '-' || c == '_'
    }
}

/// Inclusive character span of the word at character `at`, or `None` off a word.
pub fn word_at(text: &str, at: usize, big: bool) -> Option<(usize, usize)> {
    let chars: Vec<char> = text.chars().collect();
    if !word_char(*chars.get(at)?, big) {
        return None;
    }
    let lo = chars[..at].iter().rposition(|&c| !word_char(c, big)).map_or(0, |p| p + 1);
    let hi = chars[at..].iter().position(|&c| !word_char(c, big)).map_or(chars.len(), |p| at + p);
    Some((lo, hi - 1))
}

pub struct App {
    pub entries: Vec<Entry>,
    /// Each entry's own timestamp, or the last one before it. Continuation lines
    /// such as tracebacks belong to the record that started them.
    when: Vec<Option<NaiveDateTime>>,
    first_day: Option<NaiveDate>,
    pub source: String,
    /// Syslog levels, or HTTP status classes. Decided again whenever logs change.
    pub kind: LogKind,
    /// File names, indexed by `Entry::file`.
    pub files: Vec<String>,
    /// Where each file was read from; `None` is stdin.
    paths: Vec<Option<PathBuf>>,
    /// Globs typed into the add-files prompt, for the caller to load.
    pub pending_add: Option<String>,
    /// Shown severities, indexed by `Severity as usize`.
    pub levels: [bool; 5],
    pub include: Option<Query>,
    pub exclude: Option<Query>,
    pub find: Option<Query>,
    pub case_sensitive: bool,
    /// `[from, to)`.
    pub time: Option<(NaiveDateTime, NaiveDateTime)>,
    /// Entries that pass every filter, in file order.
    pub visible: Vec<usize>,
    pub patterns: Vec<Pattern>,
    pub selected: usize,
    pub line_cursor: usize,
    pub all_cursor: usize,
    pub view: View,
    /// The view `All` was entered from, for going back.
    came_from: View,
    /// First drawn row of each view's list, indexed by `View as usize`.
    pub tops: [usize; 3],
    pub page_rows: usize,
    pub wrap: bool,
    pub hscroll: usize,
    pub power_scroll: bool,
    pub mouse: bool,
    /// A name from `theme::NAMES`. Drawing falls back to terminal when it isn't.
    pub theme_name: String,
    pub bucket_secs: i64,
    pub hist_geom: Option<HistGeom>,
    pub hist_frame: Option<ScreenBox>,
    /// Longest visible raw line and pattern, in bytes. The horizontal scrollbar
    /// treats that as columns; these logs are almost all ASCII.
    pub max_raw: usize,
    pub max_pattern: usize,
    /// Anchor and current bucket while dragging across the timeline.
    pub drag: Option<(usize, usize)>,
    /// Last mouse position, for the timeline tooltip.
    pub hover: Option<(u16, u16)>,
    /// Main list rows as last drawn, top to bottom.
    pub rows: Vec<RowHit>,
    /// The list's scrollbars as last drawn, and which one is being dragged.
    pub vbar: Option<Bar>,
    pub hbar: Option<Bar>,
    pub bar_drag: Option<Axis>,
    pub selection: Option<Selection>,
    /// The left button is down and extending `selection`.
    pub selecting: bool,
    pub menu: Option<Menu>,
    pub prompt: Option<Prompt>,
    pub message: Option<String>,
    pub show_help: bool,
    pub confirm_quit: bool,
    pub should_quit: bool,
    generation: u64,
    find_generation: u64,
    hist: Option<(usize, u64, Vec<Bucket>)>,
    hits: Option<(HitsKey, Vec<usize>)>,
}

/// What find hits were computed for: view, selected pattern, filter and find generations.
type HitsKey = (View, usize, u64, u64);

impl App {
    pub fn new(entries: Vec<Entry>, source: String) -> Self {
        let when = parse::carry_forward(&entries);
        let first_day = entries.iter().find_map(|e| e.ts).map(|t| t.date());
        let prefs = config::ui();
        let mut app = App {
            entries,
            when,
            first_day,
            files: vec![source.clone()],
            paths: vec![None],
            pending_add: None,
            source,
            kind: LogKind::System,
            levels: [true; 5],
            include: None,
            exclude: None,
            find: None,
            case_sensitive: prefs.case_sensitive,
            time: None,
            visible: Vec::new(),
            patterns: Vec::new(),
            selected: 0,
            line_cursor: 0,
            all_cursor: 0,
            view: View::Patterns,
            came_from: View::Patterns,
            tops: [0; 3],
            page_rows: 10,
            wrap: prefs.wrap,
            hscroll: 0,
            power_scroll: prefs.power_scroll,
            mouse: prefs.mouse,
            theme_name: prefs.theme,
            bucket_secs: 60,
            hist_geom: None,
            hist_frame: None,
            max_raw: 0,
            max_pattern: 0,
            drag: None,
            hover: None,
            rows: Vec::new(),
            vbar: None,
            hbar: None,
            bar_drag: None,
            selection: None,
            selecting: false,
            menu: None,
            prompt: None,
            message: None,
            show_help: false,
            confirm_quit: false,
            should_quit: false,
            generation: 0,
            find_generation: 0,
            hist: None,
            hits: None,
        };
        app.rebuild();
        app
    }

    pub fn from_logs(logs: Vec<FileLog>) -> Self {
        let mut app = App::new(Vec::new(), String::new());
        app.set_logs(logs);
        app
    }

    fn passes(&self, i: usize) -> bool {
        let e = &self.entries[i];
        if !self.levels[e.severity as usize] {
            return false;
        }
        if let Some((from, to)) = self.time {
            match self.when[i] {
                Some(t) if t >= from && t < to => {}
                _ => return false,
            }
        }
        if self.include.as_ref().is_some_and(|q| !q.re.is_match(&e.raw)) {
            return false;
        }
        !self.exclude.as_ref().is_some_and(|q| q.re.is_match(&e.raw))
    }

    /// Re-apply the filters and regroup, keeping the cursor on the same pattern
    /// and line where they survive.
    pub fn rebuild(&mut self) {
        let keep_pattern = self.selected_pattern().map(|p| p.key.clone());
        let keep_line = self.selected_lines().get(self.line_cursor).copied();
        let keep_all = self.visible.get(self.all_cursor).copied();

        let visible = par_filter(self.entries.len(), |i| self.passes(i));
        self.visible = visible;

        let mut by_text: HashMap<&str, ([usize; 5], Vec<usize>)> = HashMap::new();
        let mut max_raw = 0;
        for &i in &self.visible {
            let e = &self.entries[i];
            max_raw = max_raw.max(e.raw.len());
            let slot = by_text.entry(&e.pattern).or_insert(([0; 5], Vec::new()));
            slot.0[e.severity as usize] += 1;
            slot.1.push(i);
        }
        self.max_raw = max_raw;
        let mut patterns: Vec<Pattern> = by_text
            .into_iter()
            .map(|(key, (counts, lines))| {
                let (text, masks) = parse::display_pattern(key);
                Pattern { key: key.to_string(), text, masks, count: lines.len(), severity: dominant(&counts), lines }
            })
            .collect();
        patterns.sort_by(|a, b| b.count.cmp(&a.count).then(a.text.cmp(&b.text)));
        self.max_pattern = patterns.iter().map(|p| p.text.len()).max().unwrap_or(0);
        self.patterns = patterns;

        self.selected = keep_pattern
            .and_then(|k| self.patterns.iter().position(|p| p.key == k))
            .unwrap_or(0);
        self.line_cursor = keep_line.map_or(0, |e| nearest(self.selected_lines(), e));
        self.all_cursor = keep_all.map_or(0, |e| nearest(&self.visible, e));
        self.generation += 1;
    }

    pub fn selected_pattern(&self) -> Option<&Pattern> {
        self.patterns.get(self.selected)
    }

    /// Entries behind the selected pattern.
    pub fn selected_lines(&self) -> &[usize] {
        self.selected_pattern().map_or(&[], |p| &p.lines)
    }

    /// When an entry happened: its own timestamp, or the one it inherits from
    /// the record it continues.
    pub fn time_of(&self, entry: usize) -> Option<NaiveDateTime> {
        *self.when.get(entry)?
    }

    /// In the raw-line views, when the cursor line happened. A continuation
    /// line has the time of the record it belongs to.
    pub fn cursor_time(&self) -> Option<NaiveDateTime> {
        let e = match self.view {
            View::Patterns => return None,
            View::Lines => *self.selected_lines().get(self.line_cursor)?,
            View::All => *self.visible.get(self.all_cursor)?,
        };
        self.when[e]
    }

    pub fn total_matching(&self) -> usize {
        self.visible.len()
    }

    /// Snap to the smallest ladder width that fits the span into `width` columns.
    pub fn histogram(&mut self, width: usize) -> &[Bucket] {
        let fresh = matches!(&self.hist, Some((w, g, _)) if *w == width && *g == self.generation);
        if !fresh {
            let buckets = self.compute_histogram(width);
            self.hist = Some((width, self.generation, buckets));
        }
        &self.hist.as_ref().unwrap().2
    }

    fn compute_histogram(&mut self, width: usize) -> Vec<Bucket> {
        let mut range: Option<(NaiveDateTime, NaiveDateTime)> = None;
        for ts in self.visible.iter().filter_map(|&i| self.entries[i].ts) {
            range = Some(range.map_or((ts, ts), |(lo, hi)| (lo.min(ts), hi.max(ts))));
        }
        let Some((min, max)) = range else {
            return Vec::new();
        };
        if width == 0 {
            return Vec::new();
        }

        let span = (max - min).num_seconds().max(1);
        let secs = *LADDER_SECS
            .iter()
            .find(|&&s| span / s < width as i64)
            .unwrap_or(LADDER_SECS.last().unwrap());
        self.bucket_secs = secs;

        let n = (span / secs) as usize + 1;
        let mut buckets: Vec<Bucket> = (0..n)
            .map(|i| Bucket {
                start: min + TimeDelta::seconds(i as i64 * secs),
                counts: [0; 5],
            })
            .collect();

        for &i in &self.visible {
            let e = &self.entries[i];
            let Some(ts) = e.ts else { continue };
            let idx = ((ts - min).num_seconds() / secs) as usize;
            if let Some(b) = buckets.get_mut(idx) {
                b.counts[e.severity as usize] += 1;
            }
        }
        buckets
    }

    pub fn list_len(&self) -> usize {
        match self.view {
            View::Patterns => self.patterns.len(),
            View::Lines => self.selected_lines().len(),
            View::All => self.visible.len(),
        }
    }

    pub fn cursor(&self) -> usize {
        match self.view {
            View::Patterns => self.selected,
            View::Lines => self.line_cursor,
            View::All => self.all_cursor,
        }
    }

    fn set_cursor(&mut self, at: usize) {
        let at = at.min(self.list_len().saturating_sub(1));
        match self.view {
            View::Patterns => {
                if at != self.selected {
                    self.line_cursor = 0;
                }
                self.selected = at;
            }
            View::Lines => self.line_cursor = at,
            View::All => self.all_cursor = at,
        }
    }

    /// Scroll so the cursor sits mid-screen, for jumps that land somewhere new.
    fn centre(&mut self) {
        self.tops[self.view as usize] = self.cursor().saturating_sub(self.page_rows / 2);
    }

    pub fn move_selection(&mut self, delta: isize) {
        self.set_cursor(self.cursor().saturating_add_signed(delta));
    }

    pub fn jump(&mut self, to_end: bool) {
        self.set_cursor(if to_end { usize::MAX } else { 0 });
    }

    /// Mouse wheel step: three rows, or a hundredth of the list in power scroll.
    pub fn wheel_step(&self) -> isize {
        if self.power_scroll {
            (self.list_len() / 100).max(3) as isize
        } else {
            3
        }
    }

    pub fn scroll_h(&mut self, delta: isize) {
        self.hscroll = self.hscroll.saturating_add_signed(delta);
    }

    /// Patterns open their lines, a line opens in the filtered stream around it,
    /// and a line in the stream opens the pattern it belongs to.
    pub fn enter(&mut self) {
        match self.view {
            View::Patterns => {
                if self.selected_pattern().is_some() {
                    self.view = View::Lines;
                    self.line_cursor = 0;
                }
            }
            View::Lines => {
                if let Some(&e) = self.selected_lines().get(self.line_cursor) {
                    self.came_from = View::Lines;
                    self.view = View::All;
                    self.all_cursor = nearest(&self.visible, e);
                    self.centre();
                }
            }
            View::All => {
                let Some(&e) = self.visible.get(self.all_cursor) else { return };
                let key = &self.entries[e].pattern;
                if let Some(p) = self.patterns.iter().position(|p| &p.key == key) {
                    self.selected = p;
                    self.line_cursor = nearest(self.selected_lines(), e);
                    self.view = View::Lines;
                    self.centre();
                }
            }
        }
    }

    pub fn back(&mut self) {
        self.view = match self.view {
            View::All => self.came_from,
            _ => View::Patterns,
        };
    }

    /// Clear the selection first, then the find term, then step back a view;
    /// on the pattern view, offer Settings or Files.
    pub fn escape(&mut self, screen: (u16, u16)) {
        if self.live_selection().is_some() {
            self.selection = None;
        } else if self.find.is_some() {
            self.find = None;
            self.find_generation += 1;
        } else if self.view != View::Patterns {
            self.back();
        } else {
            self.open_root_menu(screen);
        }
    }

    pub fn toggle_all_view(&mut self) {
        if self.view == View::All {
            self.back();
            return;
        }
        if let Some(&e) = self.selected_lines().get(self.line_cursor).filter(|_| self.view == View::Lines) {
            self.all_cursor = nearest(&self.visible, e);
        }
        self.came_from = self.view;
        self.view = View::All;
    }

    pub fn toggle_level(&mut self, s: Severity) {
        self.levels[s as usize] ^= true;
        self.rebuild();
    }

    /// Show only `s`; asking again brings every level back.
    pub fn solo_level(&mut self, s: Severity) {
        let mut solo = [false; 5];
        solo[s as usize] = true;
        self.levels = if self.levels == solo { [true; 5] } else { solo };
        self.rebuild();
    }

    pub fn toggle_case(&mut self) {
        self.case_sensitive ^= true;
        self.include = self.include.take().and_then(|q| self.compile_query(&q.src).ok().flatten());
        self.exclude = self.exclude.take().and_then(|q| self.compile_query(&q.src).ok().flatten());
        self.find = self.find.take().and_then(|q| self.compile_query(&q.src).ok().flatten());
        self.find_generation += 1;
        self.rebuild();
    }

    fn expand(&self, src: &str) -> String {
        let mut s = src.to_string();
        for (name, re) in &config::get().macros {
            s = Regex::new(&format!("(?i){}", regex::escape(name)))
                .unwrap()
                .replace_all(&s, NoExpand(re))
                .into_owned();
        }
        if let Some(f) = &self.find {
            s = Regex::new("(?i)%FIND%")
                .unwrap()
                .replace_all(&s, NoExpand(f.re.as_str()))
                .into_owned();
        }
        s
    }

    pub(crate) fn compile_query(&self, src: &str) -> Result<Option<Query>, String> {
        if src.is_empty() {
            return Ok(None);
        }
        let re = RegexBuilder::new(&self.expand(src))
            .case_insensitive(!self.case_sensitive)
            .build()
            .map_err(|e| format!("bad regex: {}", e.to_string().lines().last().unwrap_or_default().trim()))?;
        Ok(Some(Query { src: src.to_string(), re }))
    }

    /// `%FIND%` moves the find term into the filter, so the find is dropped.
    fn take_find_if_used(&mut self, src: &str) {
        if src.to_ascii_uppercase().contains("%FIND%") {
            self.find = None;
            self.find_generation += 1;
        }
    }

    pub fn set_include(&mut self, src: &str) -> Result<(), String> {
        self.include = self.compile_query(src)?;
        self.take_find_if_used(src);
        self.rebuild();
        Ok(())
    }

    pub fn set_exclude(&mut self, src: &str) -> Result<(), String> {
        self.exclude = self.compile_query(src)?;
        self.take_find_if_used(src);
        self.rebuild();
        Ok(())
    }

    pub fn set_find(&mut self, src: &str) -> Result<(), String> {
        let old = self.find.take();
        match self.compile_query(src) {
            Ok(q) => self.find = q,
            Err(e) => {
                self.find = old;
                return Err(e);
            }
        }
        self.find_generation += 1;
        if self.find.is_some() {
            self.seek_hit(true, true);
        }
        Ok(())
    }

    /// A typed time range, read against the log's first day. Empty is no range.
    pub fn time_range(&self, src: &str) -> Result<Option<(NaiveDateTime, NaiveDateTime)>, String> {
        if src.trim().is_empty() {
            return Ok(None);
        }
        let day = self.first_day.ok_or("this log has no timestamps")?;
        Ok(Some(parse::parse_range(src, day)?))
    }

    pub fn set_time(&mut self, src: &str) -> Result<(), String> {
        self.time = self.time_range(src)?;
        self.rebuild();
        Ok(())
    }

    /// Releasing the timeline. A drag filters to the buckets it covered. A
    /// click in a raw-line view jumps to that bucket instead; on the pattern
    /// view a click filters to the one bucket, as a drag of no distance.
    pub fn release_timeline(&mut self, a: usize, b: usize) {
        if a != b || self.view == View::Patterns {
            self.select_buckets(a, b);
        } else {
            self.jump_to_bucket(a);
        }
    }

    /// Move the cursor to the first line of the current list in `bucket`, or
    /// the nearest line by time when the bucket holds none.
    fn jump_to_bucket(&mut self, bucket: usize) {
        let Some(g) = self.hist_geom else { return };
        if self.view == View::Patterns {
            return;
        }
        let from = g.start + TimeDelta::seconds(bucket as i64 * g.secs);
        let to = from + TimeDelta::seconds(g.secs);
        let list = match self.view {
            View::Lines => self.selected_lines().to_vec(),
            _ => self.visible.clone(),
        };
        let in_bucket = |e: usize| self.when[e].is_some_and(|t| t >= from && t < to);
        let at = list.iter().position(|&e| in_bucket(e)).or_else(|| {
            let prev = list.iter().rposition(|&e| self.when[e].is_some_and(|t| t < from));
            let next = list.iter().position(|&e| self.when[e].is_some_and(|t| t >= to));
            match (prev, next) {
                (Some(p), Some(n)) => {
                    let before = (from - self.when[list[p]].unwrap()).num_seconds();
                    let after = (self.when[list[n]].unwrap() - from).num_seconds();
                    Some(if after < before { n } else { p })
                }
                (Some(p), None) => Some(p),
                (None, Some(n)) => Some(n),
                (None, None) => None,
            }
        });
        if let Some(at) = at {
            self.set_cursor(at);
            self.centre();
        }
    }

    /// Filter to the buckets between two timeline columns, inclusive.
    pub fn select_buckets(&mut self, a: usize, b: usize) {
        let Some(g) = self.hist_geom else { return };
        let (lo, hi) = (a.min(b) as i64, a.max(b) as i64);
        let from = g.start + TimeDelta::seconds(lo * g.secs);
        let to = g.start + TimeDelta::seconds((hi + 1) * g.secs);
        self.time = Some((from, to));
        self.rebuild();
    }

    pub fn go_to_line(&mut self, src: &str) -> Result<(), String> {
        let n: usize = src.trim().parse().map_err(|_| format!("{:?} is not a line number", src.trim()))?;
        if self.view == View::Patterns {
            self.came_from = View::Patterns;
            self.view = View::All;
        }
        let list = match self.view {
            View::Lines => self.selected_lines(),
            _ => &self.visible,
        };
        let at = list.partition_point(|&e| self.entries[e].line_no < n);
        self.set_cursor(at);
        self.centre();
        Ok(())
    }

    pub fn open_prompt(&mut self, kind: PromptKind) {
        let src = |q: &Option<Query>| q.as_ref().map_or(String::new(), |q| q.src.clone());
        let text = match kind {
            PromptKind::Include => src(&self.include),
            PromptKind::Exclude => src(&self.exclude),
            PromptKind::Find => src(&self.find),
            PromptKind::Time => self.time.map_or(String::new(), parse::format_range),
            PromptKind::GoTo | PromptKind::AddFiles => String::new(),
        };
        self.prompt = Some(Prompt { kind, input: text.into() });
    }

    /// Apply the prompt. On a bad entry it stays open with the error showing.
    pub fn submit_prompt(&mut self) {
        let Some(p) = self.prompt.take() else { return };
        let text = p.input.value();
        let result = match p.kind {
            PromptKind::Include => self.set_include(text),
            PromptKind::Exclude => self.set_exclude(text),
            PromptKind::Find => self.set_find(text),
            PromptKind::Time => self.set_time(text),
            PromptKind::GoTo => self.go_to_line(text),
            PromptKind::AddFiles if text.trim().is_empty() => Ok(()),
            PromptKind::AddFiles => {
                self.pending_add = Some(text.to_string());
                Ok(())
            }
        };
        if let Err(e) = result {
            self.message = Some(e);
            self.prompt = Some(p);
        }
    }

    fn list_text(&self, pos: usize) -> &str {
        match self.view {
            View::Patterns => &self.patterns[pos].text,
            View::Lines => &self.entries[self.selected_lines()[pos]].raw,
            View::All => &self.entries[self.visible[pos]].raw,
        }
    }

    /// Positions in the current list that match the find term.
    pub fn find_hits(&mut self) -> &[usize] {
        let key = (self.view, self.selected, self.generation, self.find_generation);
        if self.hits.as_ref().is_none_or(|(k, _)| *k != key) {
            let at = match &self.find {
                Some(q) => par_filter(self.list_len(), |p| q.re.is_match(self.list_text(p))),
                None => Vec::new(),
            };
            self.hits = Some((key, at));
        }
        &self.hits.as_ref().unwrap().1
    }

    /// Which hit the cursor is on, if any, and how many there are.
    pub fn find_status(&mut self) -> Option<(Option<usize>, usize)> {
        self.find.as_ref()?;
        let cur = self.cursor();
        let hits = self.find_hits();
        Some((hits.binary_search(&cur).ok(), hits.len()))
    }

    fn seek_hit(&mut self, forward: bool, include_current: bool) {
        let cur = self.cursor();
        let hits = self.find_hits();
        let target = if hits.is_empty() {
            None
        } else if forward {
            let i = hits.partition_point(|&h| if include_current { h < cur } else { h <= cur });
            hits.get(i).or(hits.first()).copied()
        } else {
            match hits.partition_point(|&h| h < cur) {
                0 => hits.last().copied(),
                i => Some(hits[i - 1]),
            }
        };
        match target {
            Some(t) => {
                self.set_cursor(t);
                self.centre();
            }
            None => self.message = Some("no matches here".into()),
        }
    }

    pub fn find_next(&mut self, forward: bool) {
        if self.find.is_none() {
            self.message = Some("nothing to find: press f".into());
            return;
        }
        self.seek_hit(forward, false);
    }

    fn sel_key(&self) -> SelKey {
        (self.view, self.selected, self.generation)
    }

    /// The selection, if it was made on the list now showing.
    pub fn live_selection(&self) -> Option<Selection> {
        self.selection.filter(|s| s.key == self.sel_key() && s.pos < self.list_len())
    }

    /// What a list item shows, as the selection counts characters.
    pub fn row_text(&self, pos: usize) -> &str {
        self.list_text(pos).trim_end()
    }

    pub fn selected_text(&self) -> Option<String> {
        let s = self.live_selection()?;
        let r = s.range();
        Some(self.row_text(s.pos).chars().skip(r.start).take(r.len()).collect())
    }

    fn row_at(&self, row: u16) -> Option<RowHit> {
        self.rows.iter().copied().find(|h| h.y == row && h.pos < self.list_len())
    }

    /// The list item and character under a screen cell. Left of the text
    /// counts as its first character, right of it as its last.
    pub fn hit(&self, col: u16, row: u16) -> Option<(usize, usize)> {
        let h = self.row_at(row)?;
        let len = self.row_text(h.pos).chars().count();
        let at = h.first + col.saturating_sub(h.x) as usize;
        Some((h.pos, at.min(len.saturating_sub(1))))
    }

    /// A press on the list: one click moves the cursor and starts a drag,
    /// two take the word under the pointer, three the whitespace-delimited one.
    /// A double click in the gutter does what Enter does: a pattern opens its
    /// lines, and a raw line opens in the all-lines view.
    pub fn click(&mut self, col: u16, row: u16, clicks: u8) {
        if clicks == 2
            && matches!(self.view, View::Patterns | View::Lines)
            && let Some(h) = self.row_at(row).filter(|h| col < h.x)
        {
            self.selection = None;
            self.selecting = false;
            self.set_cursor(h.pos);
            self.enter();
            return;
        }
        self.select_at(col, row, clicks);
    }

    fn select_at(&mut self, col: u16, row: u16, clicks: u8) {
        self.selection = None;
        let Some((pos, at)) = self.hit(col, row) else { return };
        self.set_cursor(pos);
        let span = match clicks {
            1 => Some((at, at)),
            n => word_at(self.row_text(pos), at, n >= 3),
        };
        if let Some((anchor, head)) = span {
            self.selection = Some(Selection { key: self.sel_key(), pos, anchor, head });
        }
        self.selecting = clicks == 1;
    }

    /// Extend a drag. Leaving the item clamps to its start or end.
    pub fn drag_to(&mut self, col: u16, row: u16) {
        let Some(mut s) = self.live_selection().filter(|_| self.selecting) else { return };
        let Some((pos, at)) = self.hit(col, row) else { return };
        s.head = match pos.cmp(&s.pos) {
            std::cmp::Ordering::Equal => at,
            std::cmp::Ordering::Less => 0,
            std::cmp::Ordering::Greater => self.row_text(s.pos).chars().count().saturating_sub(1),
        };
        self.selection = Some(s);
    }

    /// A click that never moved selects nothing.
    pub fn release(&mut self) {
        if self.selecting && self.selection.is_some_and(|s| s.anchor == s.head) {
            self.selection = None;
        }
        self.selecting = false;
    }

    pub fn bar_at(&self, col: u16, row: u16) -> Option<Bar> {
        [self.vbar, self.hbar].into_iter().flatten().find(|b| b.contains(col, row))
    }

    /// A press on a scrollbar: the arrows step by one, the track jumps there
    /// and starts a drag.
    pub fn press_bar(&mut self, bar: Bar, col: u16, row: u16) {
        let along = bar.along(col, row);
        let step = if along == 0 {
            -1
        } else if along + 1 == bar.cells {
            1
        } else {
            self.bar_drag = Some(bar.axis);
            self.drag_bar(col, row);
            return;
        };
        match bar.axis {
            Axis::Vertical => self.move_selection(step),
            Axis::Horizontal => self.scroll_h(step),
        }
    }

    pub fn drag_bar(&mut self, col: u16, row: u16) {
        let bar = match self.bar_drag {
            Some(Axis::Vertical) => self.vbar,
            Some(Axis::Horizontal) => self.hbar,
            None => None,
        };
        let Some(bar) = bar else { return };
        let p = bar.position_at(col, row);
        match bar.axis {
            Axis::Vertical => {
                self.set_cursor(p);
                self.centre();
            }
            Axis::Horizontal => self.hscroll = p,
        }
    }

    fn positioned(&self, entries: &[(&str, MenuAction)], text: String, col: u16, row: u16, screen: (u16, u16)) -> Menu {
        let entries = entries.iter().map(|(s, a)| (s.to_string(), *a)).collect();
        let mut menu = Menu { x: 0, y: 0, title: None, text, entries, at: 0 };
        let (x, y) = Menu::place(col, row, screen, menu.width(), menu.height());
        menu.x = x;
        menu.y = y;
        menu
    }

    /// Right click on the list: act on the selection, or on the word under the pointer.
    pub fn open_menu(&mut self, col: u16, row: u16, screen: (u16, u16)) {
        if self.selected_text().is_none() {
            self.select_at(col, row, 2);
            self.selecting = false;
        }
        let Some(text) = self.selected_text() else {
            self.message = Some("nothing selected: drag or double-click some text".into());
            return;
        };
        let entries = [("Include", MenuAction::Include), ("Exclude", MenuAction::Exclude), ("Find", MenuAction::Find)];
        self.menu = Some(self.positioned(&entries, text, col, row, screen));
    }

    /// Right click on the timeline.
    pub fn open_timeline_menu(&mut self, col: u16, row: u16, screen: (u16, u16)) {
        let entries = [("Clear time filter", MenuAction::ClearTime), ("Choose time period", MenuAction::ChooseTime)];
        self.menu = Some(self.positioned(&entries, String::new(), col, row, screen));
    }

    /// Write the theme and the Settings toggles into `config.toml`.
    pub fn save_prefs(&self) {
        config::save_ui(&config::Ui {
            theme: self.theme_name.clone(),
            wrap: self.wrap,
            mouse: self.mouse,
            case_sensitive: self.case_sensitive,
            power_scroll: self.power_scroll,
        });
    }

    /// Esc on the pattern view: Files, Settings, Themes, Quit.
    pub fn open_root_menu(&mut self, screen: (u16, u16)) {
        let entries = vec![
            ("Files".to_string(), MenuAction::OpenFiles),
            ("Settings".to_string(), MenuAction::OpenSettings),
            ("Themes".to_string(), MenuAction::OpenThemes),
            ("Quit".to_string(), MenuAction::Quit),
        ];
        self.menu = Some(Self::centred(None, entries, screen));
    }

    /// Line wrap, mouse, case and power scroll, plus the include and exclude
    /// prompts. A toggle stays open so the label shows the new state.
    pub fn open_settings_menu(&mut self, screen: (u16, u16)) {
        let mark = |on: bool| if on { "on" } else { "off" };
        let entries = vec![
            (format!("Line wrap: {}", mark(self.wrap)), MenuAction::ToggleWrap),
            (format!("Mouse: {}", mark(self.mouse)), MenuAction::ToggleMouse),
            (format!("Case sensitive: {}", mark(self.case_sensitive)), MenuAction::ToggleCase),
            (format!("Power scroll: {}", mark(self.power_scroll)), MenuAction::TogglePower),
            ("Include\u{2026}".to_string(), MenuAction::EditInclude),
            ("Exclude\u{2026}".to_string(), MenuAction::EditExclude),
            ("Clear filters".to_string(), MenuAction::ClearFilters),
        ];
        self.menu = Some(Self::centred(Some("Settings"), entries, screen));
    }

    /// Every built-in theme. The current one is marked, and the menu stays open.
    pub fn open_themes_menu(&mut self, screen: (u16, u16)) {
        let entries = theme::NAMES
            .iter()
            .enumerate()
            .map(|(i, name)| {
                let mark = if *name == self.theme_name { " *" } else { "" };
                (format!("{name}{mark}"), MenuAction::SetTheme(i as u8))
            })
            .collect();
        self.menu = Some(Self::centred(Some("Themes"), entries, screen));
    }

    /// Add or remove log files, or drop the filters. Stdin cannot be removed;
    /// when it contributed lines it is listed greyed out, so you can see it is there.
    pub fn open_files_menu(&mut self, screen: (u16, u16)) {
        let mut entries = vec![("Add files\u{2026}".to_string(), MenuAction::AddFiles)];
        let several = self.files.len() > 1;
        for (i, (name, path)) in self.files.iter().zip(&self.paths).enumerate() {
            match path {
                Some(_) if several => entries.push((format!("Remove {name}"), MenuAction::RemoveFile(i))),
                None if self.entries.iter().any(|e| e.file == i) => {
                    entries.push((name.clone(), MenuAction::Inert));
                }
                _ => {}
            }
        }
        if self.include.is_some() || self.exclude.is_some() || self.time.is_some() {
            entries.push(("Clear filters".to_string(), MenuAction::ClearFilters));
        }
        self.menu = Some(Self::centred(Some("Files"), entries, screen));
    }

    fn centred(title: Option<&'static str>, entries: Vec<(String, MenuAction)>, screen: (u16, u16)) -> Menu {
        let mut menu = Menu { x: 0, y: 0, title, text: String::new(), entries, at: 0 };
        menu.x = screen.0.saturating_sub(menu.width()) / 2;
        menu.y = screen.1.saturating_sub(menu.height()) / 2;
        menu
    }

    /// Apply the highlighted menu item. `screen` recentres a menu that stays open.
    pub fn pick_menu(&mut self, item: usize, screen: (u16, u16)) {
        if !self.menu.as_ref().is_some_and(|m| m.selectable(item)) {
            return;
        }
        let Some(menu) = self.menu.take() else { return };
        let Some(&(_, action)) = menu.entries.get(item) else { return };
        let src = regex::escape(&menu.text);
        let result = match action {
            MenuAction::Include => self.set_include(&src),
            MenuAction::Exclude => self.set_exclude(&src),
            MenuAction::Find => self.set_find(&src),
            MenuAction::ClearTime => self.set_time(""),
            MenuAction::ChooseTime => {
                self.open_prompt(PromptKind::Time);
                Ok(())
            }
            MenuAction::AddFiles => {
                self.open_prompt(PromptKind::AddFiles);
                Ok(())
            }
            MenuAction::RemoveFile(i) => {
                self.remove_file(i);
                Ok(())
            }
            MenuAction::ClearFilters => {
                self.clear_filters();
                Ok(())
            }
            MenuAction::OpenSettings => {
                self.open_settings_menu(screen);
                Ok(())
            }
            MenuAction::OpenFiles => {
                self.open_files_menu(screen);
                Ok(())
            }
            MenuAction::OpenThemes => {
                self.open_themes_menu(screen);
                Ok(())
            }
            MenuAction::SetTheme(i) => {
                if let Some(name) = theme::NAMES.get(i as usize) {
                    self.theme_name = (*name).to_string();
                    self.save_prefs();
                    self.open_themes_menu(screen);
                    self.menu.as_mut().unwrap().at = item;
                }
                Ok(())
            }
            MenuAction::Quit => {
                self.confirm_quit = true;
                Ok(())
            }
            MenuAction::ToggleWrap => {
                self.wrap ^= true;
                self.save_prefs();
                self.open_settings_menu(screen);
                self.menu.as_mut().unwrap().at = item;
                Ok(())
            }
            MenuAction::ToggleMouse => {
                self.mouse ^= true;
                self.save_prefs();
                self.open_settings_menu(screen);
                self.menu.as_mut().unwrap().at = item;
                Ok(())
            }
            MenuAction::ToggleCase => {
                self.toggle_case();
                self.save_prefs();
                self.open_settings_menu(screen);
                self.menu.as_mut().unwrap().at = item;
                Ok(())
            }
            MenuAction::TogglePower => {
                self.power_scroll ^= true;
                self.save_prefs();
                self.open_settings_menu(screen);
                self.menu.as_mut().unwrap().at = item;
                Ok(())
            }
            MenuAction::EditInclude => {
                self.open_prompt(PromptKind::Include);
                Ok(())
            }
            MenuAction::EditExclude => {
                self.open_prompt(PromptKind::Exclude);
                Ok(())
            }
            MenuAction::Inert => Ok(()),
        };
        if let Err(e) = result {
            self.message = Some(e);
        }
        if matches!(action, MenuAction::Include | MenuAction::Exclude | MenuAction::Find) {
            self.selection = None;
        }
    }

    /// Every file's entries, back as separate logs, leaving `entries` empty.
    fn take_logs(&mut self) -> Vec<FileLog> {
        let files: Vec<(String, Option<PathBuf>)> = self.files.iter().cloned().zip(self.paths.iter().cloned()).collect();
        parse::split_logs(std::mem::take(&mut self.entries), &files)
    }

    /// Replace everything loaded with `logs`, combined as at startup. Filters
    /// and toggles stay; cursors go back to the top.
    pub fn set_logs(&mut self, logs: Vec<FileLog>) {
        self.files = logs.iter().map(|f| f.name.clone()).collect();
        self.paths = logs.iter().map(|f| f.path.clone()).collect();
        let (mut entries, combined, warning) = parse::combine_logs(logs);
        self.kind = parse::classify(&mut entries);
        self.source = source_label(&self.files, combined);
        self.message = warning;
        self.when = parse::carry_forward(&entries);
        self.first_day = entries.iter().find_map(|e| e.ts).map(|t| t.date());
        self.entries = entries;
        self.visible.clear();
        self.patterns.clear();
        self.selected = 0;
        self.line_cursor = 0;
        self.all_cursor = 0;
        self.tops = [0; 3];
        self.view = View::Patterns;
        self.came_from = View::Patterns;
        self.selection = None;
        self.hist = None;
        self.hits = None;
        self.rebuild();
    }

    /// Paths already loaded, for skipping repeats when adding.
    pub fn loaded_paths(&self) -> Vec<PathBuf> {
        self.paths.iter().flatten().cloned().collect()
    }

    /// Add more files to what is loaded.
    pub fn add_logs(&mut self, more: Vec<FileLog>) {
        let n = more.len();
        let mut logs = self.take_logs();
        logs.extend(more);
        self.set_logs(logs);
        let added = format!("added {n} file{}", if n == 1 { "" } else { "s" });
        self.message = Some(match self.message.take() {
            Some(w) => format!("{added}; {w}"),
            None => added,
        });
    }

    pub fn remove_file(&mut self, i: usize) {
        if self.files.len() < 2 || i >= self.files.len() {
            self.message = Some("can't remove the only log".into());
            return;
        }
        let mut logs = self.take_logs();
        let gone = logs.remove(i).name;
        self.set_logs(logs);
        self.message = Some(match self.message.take() {
            Some(w) => format!("removed {gone}; {w}"),
            None => format!("removed {gone}"),
        });
    }

    fn clear_filters(&mut self) {
        self.include = None;
        self.exclude = None;
        self.time = None;
        self.rebuild();
        self.message = Some("filters cleared".into());
    }
}

/// The status bar's name for what is loaded.
pub fn source_label(names: &[String], combined: parse::Combined) -> String {
    match (names, combined) {
        ([one], _) => one.clone(),
        (many, parse::Combined::Merged) => format!("{} files, merged by time", many.len()),
        (many, parse::Combined::Concatenated) => format!("{} files", many.len()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::parse_lines;

    fn app_from(lines: &[&str]) -> App {
        let entries = parse_lines(lines.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        App::new(entries, "test".into())
    }

    fn raw_lines(app: &App) -> Vec<&str> {
        app.selected_lines().iter().map(|&i| app.entries[i].raw.as_str()).collect()
    }

    #[test]
    fn ranks_patterns_by_frequency() {
        let app = app_from(&[
            "2026-10-16 09:00:00 [ERR] conn 10.0.0.1 failed",
            "2026-10-16 09:00:01 [ERR] conn 10.0.0.2 failed",
            "2026-10-16 09:00:02 [INFO] started",
        ]);
        assert_eq!(app.patterns.len(), 2);
        assert_eq!(app.patterns[0].count, 2);
        assert_eq!(app.patterns[0].severity, Severity::Error);
    }

    #[test]
    fn a_stray_error_does_not_relabel_the_whole_group() {
        let mut counts = [0; 5];
        counts[Severity::Info as usize] = 260;
        counts[Severity::Error as usize] = 3;
        assert_eq!(dominant(&counts), Severity::Info);
    }

    #[test]
    fn an_even_split_is_labelled_with_the_worse_level() {
        let mut counts = [0; 5];
        counts[Severity::Warn as usize] = 5;
        counts[Severity::Info as usize] = 5;
        assert_eq!(dominant(&counts), Severity::Warn);
    }

    #[test]
    fn levelled_lines_group_apart_from_each_other() {
        // A level token survives normalisation, so error lines form their own
        // group rather than hiding among the info lines they resemble.
        let app = app_from(&[
            "2026-10-16 09:00:00 [INFO] fetched pkg-1",
            "2026-10-16 09:00:01 [INFO] fetched pkg-2",
            "2026-10-16 09:00:02 [ERROR] fetched pkg-3",
        ]);
        assert_eq!(app.patterns.len(), 2);
        assert_eq!(app.patterns[0].severity, Severity::Info);
        assert_eq!(app.patterns[0].count, 2);
        assert_eq!(app.patterns[1].severity, Severity::Error);
    }

    #[test]
    fn toggling_a_level_hides_its_lines() {
        let mut app = app_from(&["[ERR] boom", "[INFO] fine", "[INFO] also fine"]);
        app.toggle_level(Severity::Info);
        assert_eq!(app.total_matching(), 1);
        app.toggle_level(Severity::Info);
        assert_eq!(app.total_matching(), 3);
    }

    #[test]
    fn soloing_a_level_twice_shows_everything_again() {
        let mut app = app_from(&["[ERR] boom", "[INFO] fine"]);
        app.solo_level(Severity::Error);
        assert_eq!(app.total_matching(), 1);
        assert_eq!(app.patterns[0].severity, Severity::Error);
        app.solo_level(Severity::Error);
        assert_eq!(app.levels, [true; 5]);
    }

    #[test]
    fn include_and_exclude_regexes_filter_lines() {
        let mut app = app_from(&["[ERR] disk full", "[ERR] conn refused", "[INFO] Disk ok"]);
        app.set_include("disk").unwrap();
        assert_eq!(app.total_matching(), 2, "case-insensitive by default");
        app.set_exclude(r"\bok\b").unwrap();
        assert_eq!(app.total_matching(), 1);
        app.toggle_case();
        assert_eq!(app.total_matching(), 1);
        app.set_exclude("").unwrap();
        assert_eq!(app.total_matching(), 1, "only lowercase 'disk' matches now");
    }

    #[test]
    fn a_bad_regex_is_reported_and_the_old_filter_kept() {
        let mut app = app_from(&["a", "b"]);
        app.set_include("a").unwrap();
        assert!(app.set_include("(").is_err());
        assert_eq!(app.include.as_ref().unwrap().src, "a");
    }

    #[test]
    fn macros_expand_inside_a_regex() {
        let mut app = app_from(&["from 10.1.2.3 ok", "from host ok"]);
        app.set_include("from %ip%").unwrap();
        assert_eq!(app.total_matching(), 1);
    }

    #[test]
    fn the_find_macro_moves_the_find_term_into_a_filter() {
        let mut app = app_from(&["alpha", "beta"]);
        app.set_find("bet").unwrap();
        app.set_include("%FIND%").unwrap();
        assert!(app.find.is_none());
        assert_eq!(app.total_matching(), 1);
    }

    #[test]
    fn a_time_range_keeps_continuation_lines_with_their_record() {
        let mut app = app_from(&[
            "2026-10-16 09:00:00 [INFO] before",
            "2026-10-16 09:05:00 [ERR] boom",
            "  Traceback line one",
            "2026-10-16 09:10:00 [INFO] after",
        ]);
        app.set_time("09:05").unwrap();
        let raws: Vec<&str> = app.visible.iter().map(|&i| app.entries[i].raw.as_str()).collect();
        assert_eq!(raws, ["2026-10-16 09:05:00 [ERR] boom", "  Traceback line one"]);
        app.set_time("").unwrap();
        assert_eq!(app.total_matching(), 4);
    }

    #[test]
    fn a_time_filter_zooms_the_histogram() {
        let lines: Vec<String> = (0..24)
            .map(|h| format!("2026-10-16 {h:02}:00:00 [INFO] tick"))
            .collect();
        let mut app = App::new(parse_lines(lines), "test".into());
        app.histogram(60);
        let wide = app.bucket_secs;
        app.set_time("2026-10-16 10:00..2026-10-16 12:00").unwrap();
        app.histogram(60);
        assert!(app.bucket_secs < wide, "{} vs {wide}", app.bucket_secs);
        assert_eq!(app.total_matching(), 3);
    }

    #[test]
    fn dragging_across_buckets_filters_to_them() {
        let lines: Vec<String> = (0..10)
            .map(|m| format!("2026-10-16 09:{m:02}:00 [INFO] tick"))
            .collect();
        let mut app = App::new(parse_lines(lines), "test".into());
        app.hist_geom = Some(HistGeom {
            x: 0,
            y: 0,
            height: 4,
            len: 10,
            start: app.entries[0].ts.unwrap(),
            secs: 60,
        });
        app.release_timeline(5, 2);
        assert_eq!(app.total_matching(), 4, "a drag filters");
        app.release_timeline(3, 3);
        assert_eq!(app.total_matching(), 1, "a click on the pattern view filters to that bucket");
        app.set_time("").unwrap();
        app.toggle_all_view();
        app.release_timeline(7, 7);
        assert_eq!(app.cursor_time().unwrap().to_string(), "2026-10-16 09:07:00", "a click jumps");
        assert!(app.time.is_none(), "a click does not filter");
    }

    #[test]
    fn a_click_on_an_empty_bucket_lands_on_the_nearest_line() {
        let mut app = app_from(&["2026-10-16 09:00:00 [INFO] a", "2026-10-16 09:05:00 [INFO] b"]);
        app.toggle_all_view();
        app.hist_geom = Some(HistGeom {
            x: 0,
            y: 0,
            height: 4,
            len: 6,
            start: app.entries[0].ts.unwrap(),
            secs: 60,
        });
        app.release_timeline(2, 2);
        assert_eq!(app.entries[app.visible[app.all_cursor]].raw, "2026-10-16 09:00:00 [INFO] a");
        app.release_timeline(4, 4);
        assert_eq!(app.entries[app.visible[app.all_cursor]].raw, "2026-10-16 09:05:00 [INFO] b");
    }

    #[test]
    fn drills_into_the_lines_behind_a_pattern() {
        let app = app_from(&[
            "2026-10-16 09:00:00 [ERR] conn 10.0.0.1 failed",
            "2026-10-16 09:00:01 [ERR] conn 10.0.0.2 failed",
        ]);
        let lines = raw_lines(&app);
        assert_eq!(lines.len(), 2);
        assert!(lines[1].contains("10.0.0.2"));
    }

    #[test]
    fn drilling_in_survives_blank_lines() {
        let mut app = app_from(&["[ERR] first", "", "[INFO] item 3", "", "[INFO] item 5"]);
        app.selected = app.patterns.iter().position(|p| p.text.contains("INFO")).unwrap();
        assert_eq!(raw_lines(&app), ["[INFO] item 3", "[INFO] item 5"]);
    }

    #[test]
    fn enter_walks_pattern_line_context_and_back_to_pattern() {
        let mut app = app_from(&["[INFO] a 1", "[ERR] b", "[INFO] a 2"]);
        app.enter();
        assert_eq!(app.view, View::Lines);
        app.move_selection(1);
        app.enter();
        assert_eq!(app.view, View::All);
        assert_eq!(app.entries[app.visible[app.all_cursor]].raw, "[INFO] a 2");
        app.move_selection(-1);
        app.enter();
        assert_eq!(app.view, View::Lines);
        assert_eq!(raw_lines(&app), ["[ERR] b"]);
    }

    #[test]
    fn find_cycles_through_matches_and_wraps() {
        let mut app = app_from(&["x 1", "boom 2", "x 3", "boom 4"]);
        app.toggle_all_view();
        app.set_find("boom").unwrap();
        assert_eq!(app.all_cursor, 1);
        app.find_next(true);
        assert_eq!(app.all_cursor, 3);
        app.find_next(true);
        assert_eq!(app.all_cursor, 1);
        app.find_next(false);
        assert_eq!(app.all_cursor, 3);
        assert_eq!(app.find_status(), Some((Some(1), 2)));
    }

    #[test]
    fn go_to_line_lands_on_the_nearest_visible_line() {
        let mut app = app_from(&["[ERR] a", "[INFO] b", "[ERR] c", "[INFO] d"]);
        app.solo_level(Severity::Error);
        app.go_to_line("2").unwrap();
        assert_eq!(app.view, View::All);
        assert_eq!(app.entries[app.visible[app.all_cursor]].line_no, 3);
    }

    #[test]
    fn allocation_keeps_a_stray_error_visible() {
        let mut counts = [0; 5];
        counts[Severity::Info as usize] = 1000;
        counts[Severity::Error as usize] = 1;
        let a = allocate(&counts, 10);
        assert_eq!(a.iter().sum::<usize>(), 10);
        assert_eq!(a[Severity::Error as usize], 1);
    }

    #[test]
    fn allocation_is_proportional() {
        let mut counts = [0; 5];
        counts[Severity::Warn as usize] = 30;
        counts[Severity::Info as usize] = 10;
        assert_eq!(allocate(&counts, 8)[Severity::Warn as usize], 6);
    }

    #[test]
    fn histogram_picks_a_width_that_fits() {
        // One hour of entries must fit into 20 columns.
        let lines: Vec<String> = (0..60)
            .map(|m| format!("2026-10-16 09:{m:02}:00 [INFO] tick"))
            .collect();
        let mut app = App::new(parse_lines(lines), "test".into());
        let buckets = app.histogram(20);
        assert!(buckets.len() <= 20, "got {} buckets", buckets.len());
        assert_eq!(buckets.iter().map(|b| b.total()).sum::<usize>(), 60);
    }

    #[test]
    fn histogram_is_empty_without_timestamps() {
        let mut app = app_from(&["no timestamp here", "nor here"]);
        assert!(app.histogram(40).is_empty());
    }

    #[test]
    fn a_small_word_stops_at_punctuation() {
        let line = "conn to db-01_a:5432 failed";
        assert_eq!(word_at(line, 9, false), Some((8, 14)));
        assert_eq!(word_at(line, 9, true), Some((8, 19)));
        assert_eq!(word_at(line, 4, false), None, "on a space");
        assert_eq!(word_at(line, 99, false), None);
    }

    /// One drawn row per list item: text starts at column 10, unscrolled.
    fn lay_out(app: &mut App) {
        app.rows = (0..app.list_len()).map(|pos| RowHit { y: pos as u16, x: 10, pos, first: 0 }).collect();
    }

    #[test]
    fn dragging_selects_within_one_line() {
        let mut app = app_from(&["alpha beta", "gamma delta"]);
        app.toggle_all_view();
        lay_out(&mut app);
        app.click(16, 0, 1);
        app.drag_to(19, 0);
        assert_eq!(app.selected_text().as_deref(), Some("beta"));
        app.drag_to(12, 1);
        assert_eq!(app.selected_text().as_deref(), Some("beta"), "clamped to the line's end");
        app.release();
        assert_eq!(app.selected_text().as_deref(), Some("beta"));
    }

    #[test]
    fn a_plain_click_selects_nothing() {
        let mut app = app_from(&["alpha", "beta"]);
        app.toggle_all_view();
        lay_out(&mut app);
        app.click(12, 1, 1);
        app.release();
        assert_eq!(app.all_cursor, 1, "but it does move the cursor");
        assert!(app.selected_text().is_none());
    }

    #[test]
    fn double_and_triple_clicks_take_words() {
        let mut app = app_from(&["get /api/v1-x?id=3 ok"]);
        app.toggle_all_view();
        lay_out(&mut app);
        app.click(20, 0, 2);
        assert_eq!(app.selected_text().as_deref(), Some("v1-x"));
        app.click(20, 0, 3);
        assert_eq!(app.selected_text().as_deref(), Some("/api/v1-x?id=3"));
    }

    #[test]
    fn a_scrollbar_maps_its_track_onto_the_whole_range() {
        let bar = Bar { axis: Axis::Vertical, x: 50, y: 10, cells: 12, max: 99 };
        assert_eq!(bar.position_at(50, 11), 0, "first track cell");
        assert_eq!(bar.position_at(50, 20), 99, "last track cell");
        assert_eq!(bar.position_at(50, 10), 0, "the arrow clamps to the end");
        assert!(bar.contains(50, 21) && !bar.contains(50, 22) && !bar.contains(49, 15));
    }

    #[test]
    fn dragging_the_scrollbars_moves_the_view() {
        let lines: Vec<String> = (0..100).map(|i| format!("line {i}")).collect();
        let mut app = App::new(parse_lines(lines), "test".into());
        app.toggle_all_view();
        app.vbar = Some(Bar { axis: Axis::Vertical, x: 50, y: 10, cells: 12, max: 99 });
        app.hbar = Some(Bar { axis: Axis::Horizontal, x: 0, y: 30, cells: 12, max: 40 });
        app.press_bar(app.bar_at(50, 15).unwrap(), 50, 15);
        assert_eq!(app.bar_drag, Some(Axis::Vertical));
        assert_eq!(app.all_cursor, 44);
        app.drag_bar(50, 20);
        assert_eq!(app.all_cursor, 99);
        app.bar_drag = None;
        app.press_bar(app.bar_at(50, 10).unwrap(), 50, 10);
        assert_eq!(app.all_cursor, 98, "the top arrow steps up one");
        assert_eq!(app.bar_drag, None);
        app.press_bar(app.bar_at(10, 30).unwrap(), 10, 30);
        assert_eq!(app.hscroll, 40);
    }

    #[test]
    fn double_clicking_the_pattern_gutter_drills_in() {
        let mut app = app_from(&["[INFO] alpha beta", "[ERR] gamma"]);
        lay_out(&mut app);
        app.click(2, 0, 2);
        assert_eq!(app.view, View::Lines);
        assert!(app.selection.is_none());
    }

    #[test]
    fn double_clicking_a_raw_line_gutter_opens_all_lines() {
        let mut app = app_from(&["[INFO] alpha", "[ERR] gamma"]);
        lay_out(&mut app);
        app.click(2, 0, 2);
        let opened = app.entries[app.selected_lines()[0]].line_no;
        lay_out(&mut app);
        app.click(2, 0, 2);
        assert_eq!(app.view, View::All);
        assert_eq!(app.entries[app.visible[app.all_cursor]].line_no, opened);
        app.click(12, 0, 2);
        assert_eq!(app.view, View::All, "the text still selects a word");
    }

    #[test]
    fn double_clicking_pattern_text_selects_a_word() {
        let mut app = app_from(&["alpha beta"]);
        lay_out(&mut app);
        app.click(16, 0, 2);
        assert_eq!(app.view, View::Patterns);
        assert_eq!(app.selected_text().as_deref(), Some("beta"));
    }

    #[test]
    fn the_timeline_menu_clears_or_prompts_for_a_range() {
        let mut app = app_from(&["2026-10-16 09:00:00 [INFO] a", "2026-10-16 10:00:00 [INFO] b"]);
        app.set_time("09:00").unwrap();
        app.open_timeline_menu(5, 5, (80, 24));
        let menu = app.menu.as_ref().unwrap();
        assert_eq!(menu.items()[0], "Clear time filter");
        assert_eq!(menu.key_item('c'), None, "both items start with C");
        app.pick_menu(0, (80, 24));
        assert!(app.time.is_none());
        assert!(app.menu.is_none());
        app.open_timeline_menu(5, 5, (80, 24));
        app.pick_menu(1, (80, 24));
        assert!(matches!(app.prompt.as_ref().map(|p| p.kind), Some(PromptKind::Time)));
    }

    #[test]
    fn the_menu_filters_on_the_selection_literally() {
        let mut app = app_from(&["[INFO] a.b done", "[INFO] axb done", "[INFO] other"]);
        app.toggle_all_view();
        lay_out(&mut app);
        app.click(17, 0, 3);
        app.open_menu(17, 0, (80, 24));
        assert_eq!(app.menu.as_ref().unwrap().text, "a.b");
        app.pick_menu(0, (80, 24));
        assert_eq!(app.total_matching(), 1, "the dot is not a wildcard");
        assert!(app.selection.is_none());
    }

    #[test]
    fn a_selection_dies_with_the_list_it_was_made_on() {
        let mut app = app_from(&["[ERR] a", "[INFO] b"]);
        app.toggle_all_view();
        lay_out(&mut app);
        app.click(16, 0, 2);
        assert!(app.live_selection().is_some());
        app.toggle_level(Severity::Info);
        assert!(app.live_selection().is_none());
    }

    fn log(name: &str, lines: &[&str]) -> FileLog {
        let entries = parse_lines(lines.iter().map(|s| s.to_string()).collect());
        FileLog { name: name.into(), path: Some(PathBuf::from(format!("/logs/{name}"))), entries }
    }

    fn all_raw(app: &App) -> Vec<&str> {
        app.entries.iter().map(|e| e.raw.as_str()).collect()
    }

    #[test]
    fn removing_a_file_recombines_the_rest() {
        let a = log("a.log", &["2026-10-16 09:00:00 [INFO] a1", "2026-10-16 09:00:20 [INFO] a2"]);
        let b = log("b.log", &["2026-10-16 09:00:10 [INFO] b1"]);
        let c = log("c.log", &["2026-10-16 09:00:30 [INFO] c1", "  continued"]);
        let mut app = App::from_logs(vec![a, b, c]);
        assert_eq!(all_raw(&app), ["2026-10-16 09:00:00 [INFO] a1", "2026-10-16 09:00:10 [INFO] b1", "2026-10-16 09:00:20 [INFO] a2", "2026-10-16 09:00:30 [INFO] c1", "  continued"]);
        app.remove_file(1);
        assert_eq!(app.files, ["a.log", "c.log"]);
        assert_eq!(app.source, "2 files");
        assert_eq!(all_raw(&app), ["2026-10-16 09:00:00 [INFO] a1", "2026-10-16 09:00:20 [INFO] a2", "2026-10-16 09:00:30 [INFO] c1", "  continued"]);
        assert_eq!(app.entries.iter().map(|e| (e.file, e.line_no)).collect::<Vec<_>>(), [(0, 1), (0, 2), (1, 3), (1, 4)]);
        assert_eq!(app.message.as_deref(), Some("removed b.log"));
    }

    #[test]
    fn adding_files_merges_them_in_and_keeps_filters() {
        let mut app = App::from_logs(vec![log("a.log", &["2026-10-16 09:00:00 [INFO] a1", "2026-10-16 09:00:20 [ERR] a2"])]);
        app.toggle_level(Severity::Info);
        app.add_logs(vec![log("b.log", &["2026-10-16 09:00:10 [ERR] b1"])]);
        assert_eq!(app.source, "2 files, merged by time");
        assert_eq!(app.total_matching(), 2, "the level filter still applies");
        assert_eq!(app.message.as_deref(), Some("added 1 file"));
        assert_eq!(app.loaded_paths().len(), 2);
    }

    #[test]
    fn the_only_log_cannot_be_removed() {
        let mut app = App::from_logs(vec![log("a.log", &["x"])]);
        app.remove_file(0);
        assert_eq!(app.entries.len(), 1);
    }

    #[test]
    fn the_settings_menu_toggles_and_edits_filters() {
        let mut app = app_from(&["[INFO] alpha", "[INFO] beta"]);
        let screen = (80, 24);
        app.escape(screen);
        app.pick_menu(1, screen);
        assert_eq!(
            app.menu.as_ref().unwrap().items(),
            [
                "Line wrap: off",
                "Mouse: on",
                "Case sensitive: off",
                "Power scroll: off",
                "Include\u{2026}",
                "Exclude\u{2026}",
                "Clear filters",
            ]
        );
        app.pick_menu(0, screen);
        assert!(app.wrap);
        assert_eq!(app.menu.as_ref().unwrap().items()[0], "Line wrap: on");
        assert_eq!(app.menu.as_ref().unwrap().at, 0, "a toggle leaves the menu open");
        app.pick_menu(1, screen);
        assert!(!app.mouse);
        app.pick_menu(2, screen);
        assert!(app.case_sensitive);
        app.pick_menu(3, screen);
        assert!(app.power_scroll);
        app.pick_menu(4, screen);
        assert!(app.menu.is_none());
        assert!(matches!(app.prompt.as_ref().map(|p| p.kind), Some(PromptKind::Include)));
        app.prompt.as_mut().unwrap().input = "alpha".into();
        app.submit_prompt();
        assert_eq!(app.total_matching(), 1);
        app.escape(screen);
        app.pick_menu(1, screen);
        app.pick_menu(5, screen);
        assert!(matches!(app.prompt.as_ref().map(|p| p.kind), Some(PromptKind::Exclude)));
        app.prompt = None;
        app.escape(screen);
        app.pick_menu(1, screen);
        app.pick_menu(6, screen);
        assert!(app.include.is_none());
        assert!(app.menu.is_none());
        assert_eq!(app.total_matching(), 2);
    }

    #[test]
    fn the_theme_menu_marks_the_current_one_and_quit_asks() {
        let mut app = app_from(&["[INFO] a"]);
        let screen = (80, 24);
        app.escape(screen);
        app.pick_menu(2, screen);
        let items = app.menu.as_ref().unwrap().items();
        assert_eq!(items.len(), theme::NAMES.len());
        assert!(items.contains(&"terminal *"));
        let dracula = theme::NAMES.iter().position(|n| *n == "dracula").unwrap();
        app.pick_menu(dracula, screen);
        assert_eq!(app.theme_name, "dracula");
        assert_eq!(app.menu.as_ref().unwrap().items()[dracula], "dracula *");
        assert_eq!(app.menu.as_ref().unwrap().at, dracula);
        app.menu = None;
        app.escape(screen);
        app.pick_menu(3, screen);
        assert!(app.confirm_quit);
        assert!(app.menu.is_none());
    }

    #[test]
    fn escape_on_the_pattern_view_opens_the_files_menu() {
        let mut app = App::from_logs(vec![log("a.log", &["x"]), log("b.log", &["y"])]);
        app.escape((80, 24));
        assert_eq!(app.menu.as_ref().unwrap().items(), ["Files", "Settings", "Themes", "Quit"]);
        app.pick_menu(0, (80, 24));
        let menu = app.menu.as_ref().unwrap();
        assert_eq!(menu.items(), ["Add files\u{2026}", "Remove a.log", "Remove b.log"]);
        app.pick_menu(2, (80, 24));
        assert_eq!(app.files, ["a.log"]);
        app.set_include("x").unwrap();
        app.escape((80, 24));
        app.pick_menu(0, (80, 24));
        assert_eq!(app.menu.as_ref().unwrap().items(), ["Add files\u{2026}", "Clear filters"], "nothing to remove from one log");
        app.pick_menu(0, (80, 24));
        assert!(matches!(app.prompt.as_ref().map(|p| p.kind), Some(PromptKind::AddFiles)));
        app.prompt.as_mut().unwrap().input = "~/logs/*.log".into();
        app.submit_prompt();
        assert_eq!(app.pending_add.as_deref(), Some("~/logs/*.log"));
    }

    #[test]
    fn stdin_is_listed_greyed_and_cannot_be_chosen() {
        let mut stdin = log("stdin", &["x"]);
        stdin.path = None;
        let mut app = App::from_logs(vec![stdin, log("b.log", &["y"])]);
        app.escape((80, 24));
        app.pick_menu(0, (80, 24));
        let menu = app.menu.as_ref().unwrap();
        assert_eq!(menu.items(), ["Add files\u{2026}", "stdin", "Remove b.log"]);
        assert!(!menu.selectable(1));
        app.menu.as_mut().unwrap().move_at(true);
        app.menu.as_mut().unwrap().move_at(true);
        assert_eq!(app.menu.as_ref().unwrap().at, 2, "the cursor steps over stdin");
        app.pick_menu(1, (80, 24));
        assert!(app.menu.is_some(), "choosing stdin leaves the menu up");
        assert_eq!(app.files, ["stdin", "b.log"]);
    }

    #[test]
    fn a_lone_stdin_is_still_listed() {
        let mut stdin = log("stdin", &["x"]);
        stdin.path = None;
        let mut app = App::from_logs(vec![stdin]);
        app.escape((80, 24));
        app.pick_menu(0, (80, 24));
        let menu = app.menu.as_ref().unwrap();
        assert_eq!(menu.items(), ["Add files\u{2026}", "stdin"]);
        assert!(!menu.selectable(1));
        assert!(menu.key_item('s').is_none(), "its initial does not choose it");
    }

    #[test]
    fn patterns_show_mask_names_and_keep_grouping_by_key() {
        let app = app_from(&["[ERR] conn 10.0.0.1 failed", "[ERR] conn 10.0.0.2 failed"]);
        let p = &app.patterns[0];
        assert_eq!(p.text, "[ERR] conn ADDR failed");
        assert_eq!(&p.text[p.masks[0].clone()], "ADDR");
        assert_eq!(p.count, 2);
    }

    #[test]
    fn the_cursor_time_follows_raw_lines_only() {
        let mut app = app_from(&["2026-10-16 09:00:00 [ERR] boom", "  at frame", "2026-10-16 09:05:00 [INFO] ok"]);
        assert_eq!(app.cursor_time(), None, "not on the pattern view");
        app.toggle_all_view();
        app.move_selection(1);
        assert_eq!(app.cursor_time().unwrap().to_string(), "2026-10-16 09:00:00", "a continuation keeps its record's time");
        app.move_selection(1);
        assert_eq!(app.cursor_time().unwrap().to_string(), "2026-10-16 09:05:00");
    }

    #[test]
    fn selection_stays_in_bounds() {
        let mut app = app_from(&["[ERR] a", "[ERR] b"]);
        app.move_selection(100);
        assert_eq!(app.selected, app.patterns.len() - 1);
        app.move_selection(-100);
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn an_access_log_is_classed_by_http_status() {
        let lines = [
            r#"127.0.0.1 - - [20/Oct/2026:09:00:00 +0000] "GET /a HTTP/1.1" 500 10"#,
            r#"127.0.0.1 - - [20/Oct/2026:09:00:01 +0000] "POST /b HTTP/1.1" 404 10"#,
            r#"127.0.0.1 - - [20/Oct/2026:09:00:02 +0000] "GET /c HTTP/1.1" 302 10"#,
            r#"127.0.0.1 - - [20/Oct/2026:09:00:03 +0000] "GET /d HTTP/1.1" 200 10"#,
            r#"127.0.0.1 - - [20/Oct/2026:09:00:04 +0000] "GET /e HTTP/1.1" 101 10"#,
            "traceback follows",
        ];
        let mut app = App::from_logs(vec![log("access.log", &lines)]);
        assert_eq!(app.kind, LogKind::Web);
        let got: Vec<_> = app.entries.iter().map(|e| e.severity).collect();
        assert_eq!(got, [Severity::Error, Severity::Warn, Severity::Info, Severity::Info, Severity::Debug, Severity::Other]);
        app.toggle_level(Severity::Info);
        assert_eq!(app.total_matching(), 4, "2xx and 3xx are the same filter");
    }

    #[test]
    fn a_syslog_that_quotes_one_request_stays_a_system_log() {
        let lines = [
            "2026-10-16 09:00:00 [ERROR] disk full",
            r#"2026-10-16 09:00:01 [ERROR] saw "GET / HTTP/1.1" 200 in the body"#,
            "2026-10-16 09:00:02 [WARN] slow",
            "2026-10-16 09:00:03 [INFO] ok",
        ];
        let app = App::from_logs(vec![log("app.log", &lines)]);
        assert_eq!(app.kind, LogKind::System);
        assert_eq!(app.entries[1].severity, Severity::Error);
    }
}
