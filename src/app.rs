use crate::parse::{Entry, Severity};
use chrono::{NaiveDateTime, TimeDelta};
use std::collections::HashMap;

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

pub struct Pattern {
    pub text: String,
    pub count: usize,
    pub severity: Severity,
    pub lines: Vec<usize>,
}

pub struct Bucket {
    pub start: NaiveDateTime,
    pub counts: [usize; 5],
}

impl Bucket {
    pub fn total(&self) -> usize {
        self.counts.iter().sum()
    }
}

#[derive(PartialEq, Clone, Copy)]
pub enum View {
    Patterns,
    Lines,
}

pub struct App {
    pub entries: Vec<Entry>,
    pub source: String,
    pub filter: Option<Severity>,
    pub patterns: Vec<Pattern>,
    pub selected: usize,
    pub view: View,
    pub line_scroll: usize,
    pub bucket_secs: i64,
    pub should_quit: bool,
}

impl App {
    pub fn new(entries: Vec<Entry>, source: String) -> Self {
        let mut app = App {
            entries,
            source,
            filter: None,
            patterns: Vec::new(),
            selected: 0,
            view: View::Patterns,
            line_scroll: 0,
            bucket_secs: 60,
            should_quit: false,
        };
        app.rebuild();
        app
    }

    fn matches(&self, e: &Entry) -> bool {
        self.filter.is_none_or(|f| e.severity == f)
    }

    /// Group entries by normalised shape, ranked by frequency.
    pub fn rebuild(&mut self) {
        let mut by_text: HashMap<&str, ([usize; 5], Vec<usize>)> = HashMap::new();
        for e in self.entries.iter().filter(|e| self.matches(e)) {
            let slot = by_text.entry(&e.pattern).or_insert(([0; 5], Vec::new()));
            slot.0[e.severity as usize] += 1;
            slot.1.push(e.line_no);
        }

        let mut patterns: Vec<Pattern> = by_text
            .into_iter()
            .map(|(text, (counts, lines))| Pattern {
                text: text.to_string(),
                count: lines.len(),
                severity: dominant(&counts),
                lines,
            })
            .collect();
        patterns.sort_by(|a, b| b.count.cmp(&a.count).then(a.text.cmp(&b.text)));

        self.patterns = patterns;
        self.selected = self.selected.min(self.patterns.len().saturating_sub(1));
        self.line_scroll = 0;
    }

    pub fn selected_pattern(&self) -> Option<&Pattern> {
        self.patterns.get(self.selected)
    }

    /// Raw lines behind the selected pattern.
    pub fn selected_lines(&self) -> Vec<&Entry> {
        let Some(p) = self.selected_pattern() else {
            return Vec::new();
        };
        p.lines
            .iter()
            .filter_map(|n| self.entries.get(n - 1))
            .collect()
    }

    pub fn total_matching(&self) -> usize {
        self.patterns.iter().map(|p| p.count).sum()
    }

    /// Snap to the smallest ladder width that fits the span into `width` columns.
    pub fn histogram(&mut self, width: usize) -> Vec<Bucket> {
        let times: Vec<NaiveDateTime> = self
            .entries
            .iter()
            .filter(|e| self.matches(e))
            .filter_map(|e| e.ts)
            .collect();
        let (Some(&min), Some(&max)) = (times.iter().min(), times.iter().max()) else {
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

        for e in self.entries.iter().filter(|e| self.matches(e)) {
            let Some(ts) = e.ts else { continue };
            let idx = ((ts - min).num_seconds() / secs) as usize;
            if let Some(b) = buckets.get_mut(idx) {
                b.counts[e.severity as usize] += 1;
            }
        }
        buckets
    }

    pub fn move_selection(&mut self, delta: isize) {
        match self.view {
            View::Patterns => {
                if self.patterns.is_empty() {
                    return;
                }
                let last = self.patterns.len() - 1;
                self.selected = self.selected.saturating_add_signed(delta).min(last);
                self.line_scroll = 0;
            }
            View::Lines => {
                let last = self.selected_lines().len().saturating_sub(1);
                self.line_scroll = self.line_scroll.saturating_add_signed(delta).min(last);
            }
        }
    }

    pub fn cycle_filter(&mut self) {
        self.filter = match self.filter {
            None => Some(Severity::Error),
            Some(Severity::Error) => Some(Severity::Warn),
            Some(Severity::Warn) => Some(Severity::Info),
            Some(Severity::Info) => Some(Severity::Debug),
            Some(Severity::Debug) => Some(Severity::Other),
            Some(Severity::Other) => None,
        };
        self.selected = 0;
        self.rebuild();
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
    fn filtering_narrows_patterns() {
        let mut app = app_from(&[
            "2026-10-16 09:00:00 [ERR] boom",
            "2026-10-16 09:00:01 [INFO] fine",
        ]);
        app.cycle_filter(); // None -> Error
        assert_eq!(app.total_matching(), 1);
        assert_eq!(app.patterns[0].severity, Severity::Error);
    }

    #[test]
    fn filter_cycles_back_to_unfiltered() {
        let mut app = app_from(&["2026-10-16 09:00:00 [ERR] boom"]);
        for _ in 0..6 {
            app.cycle_filter();
        }
        assert!(app.filter.is_none());
    }

    #[test]
    fn drills_into_the_lines_behind_a_pattern() {
        let app = app_from(&[
            "2026-10-16 09:00:00 [ERR] conn 10.0.0.1 failed",
            "2026-10-16 09:00:01 [ERR] conn 10.0.0.2 failed",
        ]);
        let lines = app.selected_lines();
        assert_eq!(lines.len(), 2);
        assert!(lines[1].raw.contains("10.0.0.2"));
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
    fn selection_stays_in_bounds() {
        let mut app = app_from(&["[ERR] a", "[ERR] b"]);
        app.move_selection(100);
        assert_eq!(app.selected, app.patterns.len() - 1);
        app.move_selection(-100);
        assert_eq!(app.selected, 0);
    }
}
