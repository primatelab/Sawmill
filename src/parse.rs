use crate::config;
use chrono::{Datelike, NaiveDate, NaiveDateTime, NaiveTime, TimeDelta, Utc};
use regex::Regex;
use std::path::PathBuf;
use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Severity {
    Error,
    Warn,
    Info,
    Debug,
    Other,
}

/// Whether lines are classed by a syslog level token or an HTTP status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogKind {
    System,
    Web,
}

impl Severity {
    pub const ALL: [Severity; 5] = [
        Severity::Error,
        Severity::Warn,
        Severity::Info,
        Severity::Debug,
        Severity::Other,
    ];

    /// A level by name. Syslog names and HTTP classes both work, in either kind:
    /// `5xx` is `ERROR`, `4xx` is `WARN`, `3xx` and `2xx` are `INFO`, `1xx` is `DEBUG`.
    pub fn from_name(name: &str) -> Option<Severity> {
        let name = name.to_ascii_uppercase();
        let hit = Severity::ALL.into_iter().find(|s| s.label() == name);
        hit.or(match name.as_str() {
            "5XX" => Some(Severity::Error),
            "4XX" => Some(Severity::Warn),
            "3XX" | "2XX" => Some(Severity::Info),
            "1XX" => Some(Severity::Debug),
            _ => None,
        })
    }

    pub fn label(self) -> &'static str {
        match self {
            Severity::Error => "ERROR",
            Severity::Warn => "WARN",
            Severity::Info => "INFO",
            Severity::Debug => "DEBUG",
            Severity::Other => "OTHER",
        }
    }

    /// The gutter and status-bar name. On a web log, `2xx` covers 3xx as well.
    pub fn label_for(self, kind: LogKind) -> &'static str {
        match kind {
            LogKind::System => self.label(),
            LogKind::Web => match self {
                Severity::Error => "5xx",
                Severity::Warn => "4xx",
                Severity::Info => "2xx",
                Severity::Debug => "1xx",
                Severity::Other => "OTHER",
            },
        }
    }
}

#[derive(Debug, Clone)]
pub struct Entry {
    pub line_no: usize,
    /// Index of the file this line came from, in command-line order.
    pub file: usize,
    /// Line number within that file, before `combine_logs` renumbers.
    pub file_line: usize,
    pub raw: String,
    pub ts: Option<NaiveDateTime>,
    pub severity: Severity,
    pub pattern: String,
}

struct Patterns {
    severity: Regex,
    http: Regex,
    iso: Regex,
    syslog: Regex,
    clf: Regex,
    bare_time: Regex,
    space: Regex,
}

fn patterns() -> &'static Patterns {
    static P: OnceLock<Patterns> = OnceLock::new();
    P.get_or_init(|| Patterns {
        // A level is a delimited token. Requiring a delimiter either side keeps
        // identifiers such as `libgpg-error` and prose such as `"Warning: ..."`
        // from being read as this line's severity.
        severity: Regex::new(
            r"(?i)(?:^|[\s\[\(<\|\{])(ERR|ERROR|CRIT|CRITICAL|FATAL|SEVERE|PANIC|WARN|WARNING|INFO|NOTICE|DEBUG|TRACE)(?:[\s\]\)>\|\},:]|$)",
        )
        .unwrap(),
        // A quoted request, then the status. The method keeps this off ordinary
        // prose that happens to contain a 3-digit number.
        http: Regex::new(
            r#""(?:GET|POST|PUT|PATCH|DELETE|HEAD|OPTIONS|CONNECT|TRACE) [^"]*"\s+([1-5]\d{2})\b"#,
        )
        .unwrap(),
        // 2026-10-16T09:21:29 or 2026/10/16 09:21:29
        iso: Regex::new(
            r"(\d{4})[-/](\d{2})[-/](\d{2})[T ](\d{2}):(\d{2}):(\d{2})",
        )
        .unwrap(),
        // Oct 16 09:21:29
        syslog: Regex::new(
            r"(?i)\b(Jan|Feb|Mar|Apr|May|Jun|Jul|Aug|Sep|Oct|Nov|Dec)\s+(\d{1,2})\s+(\d{2}):(\d{2}):(\d{2})",
        )
        .unwrap(),
        // [16/Oct/2026:09:21:29
        clf: Regex::new(
            r"(?i)(\d{2})/(Jan|Feb|Mar|Apr|May|Jun|Jul|Aug|Sep|Oct|Nov|Dec)/(\d{4}):(\d{2}):(\d{2}):(\d{2})",
        )
        .unwrap(),
        bare_time: Regex::new(r"\b(\d{2}):(\d{2}):(\d{2})\b").unwrap(),
        space: Regex::new(r"\s+").unwrap(),
    })
}

fn month_num(m: &str) -> u32 {
    match m.to_ascii_lowercase().as_str() {
        "jan" => 1,
        "feb" => 2,
        "mar" => 3,
        "apr" => 4,
        "may" => 5,
        "jun" => 6,
        "jul" => 7,
        "aug" => 8,
        "sep" => 9,
        "oct" => 10,
        "nov" => 11,
        _ => 12,
    }
}

/// A log line's own timestamp sits at the start. Dates further in belong to the
/// payload — service status, package metadata, quoted output — and reading those
/// drags the timeline across years that the log never covers.
const STAMP_OFFSET: usize = 4;
/// Common Log Format puts the client address first, so its date starts later.
/// Its `[dd/Mon/yyyy:hh:mm:ss` shape is distinctive enough to trust further in.
const CLF_OFFSET: usize = 64;

fn head(line: &str) -> &str {
    let end = line.char_indices().nth(96).map_or(line.len(), |(i, _)| i);
    &line[..end]
}

fn num<T: std::str::FromStr>(c: &regex::Captures, i: usize) -> Option<T> {
    c[i].parse().ok()
}

/// Capture only if the whole match begins within `offset` characters.
fn near_start<'a>(re: &Regex, h: &'a str, offset: usize) -> Option<regex::Captures<'a>> {
    let c = re.captures(h)?;
    (c.get(0)?.start() <= offset).then_some(c)
}

pub fn parse_timestamp(line: &str, fallback_year: i32) -> Option<NaiveDateTime> {
    let p = patterns();
    let h = head(line);

    if let Some(c) = near_start(&p.iso, h, STAMP_OFFSET) {
        return NaiveDate::from_ymd_opt(num(&c, 1)?, num(&c, 2)?, num(&c, 3)?)?
            .and_hms_opt(num(&c, 4)?, num(&c, 5)?, num(&c, 6)?);
    }
    if let Some(c) = near_start(&p.clf, h, CLF_OFFSET) {
        return NaiveDate::from_ymd_opt(num(&c, 3)?, month_num(&c[2]), num(&c, 1)?)?
            .and_hms_opt(num(&c, 4)?, num(&c, 5)?, num(&c, 6)?);
    }
    if let Some(c) = near_start(&p.syslog, h, STAMP_OFFSET) {
        return NaiveDate::from_ymd_opt(fallback_year, month_num(&c[1]), num(&c, 2)?)?
            .and_hms_opt(num(&c, 3)?, num(&c, 4)?, num(&c, 5)?);
    }
    if let Some(c) = near_start(&p.bare_time, h, STAMP_OFFSET) {
        let t = NaiveTime::from_hms_opt(num(&c, 1)?, num(&c, 2)?, num(&c, 3)?)?;
        return Some(NaiveDate::from_ymd_opt(fallback_year, 1, 1)?.and_time(t));
    }
    None
}

pub fn parse_severity(line: &str) -> Severity {
    severity_token(line).map_or(Severity::Other, |(_, s)| s)
}

/// The byte range of the level token that decides a line's severity.
pub fn severity_token(line: &str) -> Option<(std::ops::Range<usize>, Severity)> {
    let m = patterns().severity.captures(line)?.get(1)?;
    let s = match m.as_str().to_ascii_uppercase().as_str() {
        "ERR" | "ERROR" | "CRIT" | "CRITICAL" | "FATAL" | "SEVERE" | "PANIC" => Severity::Error,
        "WARN" | "WARNING" => Severity::Warn,
        "INFO" | "NOTICE" => Severity::Info,
        "DEBUG" | "TRACE" => Severity::Debug,
        _ => Severity::Other,
    };
    Some((m.range(), s))
}

/// The status code after a quoted HTTP request, and the class it belongs to.
/// `2xx` and `3xx` share `Info`; `1xx` is `Debug`.
pub fn http_status(line: &str) -> Option<(std::ops::Range<usize>, Severity)> {
    let code = patterns().http.captures(line)?.get(1)?;
    let s = match code.as_str().as_bytes()[0] {
        b'5' => Severity::Error,
        b'4' => Severity::Warn,
        b'3' | b'2' => Severity::Info,
        b'1' => Severity::Debug,
        _ => Severity::Other,
    };
    Some((code.range(), s))
}

/// Web when access-log statuses outnumber syslog level tokens, and there are at
/// least a few of them, so one quoted request inside a syslog stays a system log.
/// Rewrites `severity` to match. Pattern keys are left alone.
pub fn classify(entries: &mut [Entry]) -> LogKind {
    let mut web = 0usize;
    let mut sys = 0usize;
    for e in entries.iter() {
        if http_status(&e.raw).is_some() {
            web += 1;
        }
        if severity_token(&e.raw).is_some() {
            sys += 1;
        }
    }
    let kind = if web >= 3 && web > sys { LogKind::Web } else { LogKind::System };
    for e in entries.iter_mut() {
        e.severity = match kind {
            LogKind::Web => http_status(&e.raw).map_or(Severity::Other, |(_, s)| s),
            LogKind::System => parse_severity(&e.raw),
        };
    }
    kind
}

struct Bounds {
    date: Regex,
    syslog: Regex,
    time: Regex,
    relative: Regex,
}

fn bounds() -> &'static Bounds {
    static B: OnceLock<Bounds> = OnceLock::new();
    B.get_or_init(|| Bounds {
        date: Regex::new(r"^(\d{4})[-/](\d{2})[-/](\d{2})(?:[T ]+(\d{1,2}):(\d{2})(?::(\d{2}))?)?$")
            .unwrap(),
        syslog: Regex::new(
            r"(?i)^(Jan|Feb|Mar|Apr|May|Jun|Jul|Aug|Sep|Oct|Nov|Dec)\s+(\d{1,2})(?:\s+(\d{1,2}):(\d{2})(?::(\d{2}))?)?$",
        )
        .unwrap(),
        time: Regex::new(r"^(\d{1,2}):(\d{2})(?::(\d{2}))?$").unwrap(),
        relative: Regex::new(r"^\+(\d+)([smhd])$").unwrap(),
    })
}

/// A typed bound and how much time it names: `15:55` is a whole minute, a bare
/// date a whole day, `15:55:07` one second.
fn parse_bound(s: &str, day: NaiveDate) -> Option<(NaiveDateTime, TimeDelta)> {
    let b = bounds();
    if let Some(c) = b.date.captures(s) {
        let d = NaiveDate::from_ymd_opt(num(&c, 1)?, num(&c, 2)?, num(&c, 3)?)?;
        return with_time(d, &c, 4);
    }
    if let Some(c) = b.syslog.captures(s) {
        let d = NaiveDate::from_ymd_opt(day.year(), month_num(&c[1]), num(&c, 2)?)?;
        return with_time(d, &c, 3);
    }
    let c = b.time.captures(s)?;
    with_time(day, &c, 1)
}

fn with_time(d: NaiveDate, c: &regex::Captures, i: usize) -> Option<(NaiveDateTime, TimeDelta)> {
    let Some(h) = c.get(i) else {
        return Some((d.and_hms_opt(0, 0, 0)?, TimeDelta::days(1)));
    };
    let (h, m) = (h.as_str().parse().ok()?, num(c, i + 1)?);
    match c.get(i + 2) {
        Some(s) => Some((d.and_hms_opt(h, m, s.as_str().parse().ok()?)?, TimeDelta::seconds(1))),
        None => Some((d.and_hms_opt(h, m, 0)?, TimeDelta::minutes(1))),
    }
}

fn relative(s: &str) -> Option<TimeDelta> {
    let c = bounds().relative.captures(s)?;
    let n: i64 = num(&c, 1)?;
    Some(match &c[2] {
        "s" => TimeDelta::seconds(n),
        "m" => TimeDelta::minutes(n),
        "h" => TimeDelta::hours(n),
        _ => TimeDelta::days(n),
    })
}

/// Read a typed time range as `[from, to)`.
///
/// `FROM..TO` with either side optional, or one bound alone for the minute or day
/// it names. The end is inclusive of what it names, so `15:55..15:56` runs to
/// 15:57:00. `TO` may be `+30s`, `+5m`, `+2h` or `+1d` after `FROM`. A time with
/// no date falls on `day`, or on `FROM`'s day for `TO`, rolling past midnight
/// when it would otherwise end before it starts.
pub fn parse_range(s: &str, day: NaiveDate) -> Result<(NaiveDateTime, NaiveDateTime), String> {
    let bad = |b: &str| format!("can't read {b:?} as a time");
    let Some((a, b)) = s.split_once("..") else {
        let s = s.trim();
        let (t, span) = parse_bound(s, day).ok_or_else(|| bad(s))?;
        return Ok((t, t + span));
    };
    let (a, b) = (a.trim(), b.trim());
    let from = match a {
        "" => None,
        _ => Some(parse_bound(a, day).ok_or_else(|| bad(a))?.0),
    };
    let to = if b.is_empty() {
        NaiveDateTime::MAX
    } else if let Some(d) = relative(b) {
        from.ok_or("a +duration needs a start time")? + d
    } else {
        let (t, span) = parse_bound(b, from.map_or(day, |f| f.date())).ok_or_else(|| bad(b))?;
        match from {
            Some(f) if t + span <= f && bounds().time.is_match(b) => t + span + TimeDelta::days(1),
            _ => t + span,
        }
    };
    let from = from.unwrap_or(NaiveDateTime::MIN);
    if to <= from {
        return Err("that range ends before it starts".into());
    }
    Ok((from, to))
}

/// The inverse of `parse_range`, for editing a range that is already set.
pub fn format_range((from, to): (NaiveDateTime, NaiveDateTime)) -> String {
    let fmt = "%Y-%m-%d %H:%M:%S";
    let a = if from == NaiveDateTime::MIN { String::new() } else { from.format(fmt).to_string() };
    let b = if to == NaiveDateTime::MAX {
        String::new()
    } else {
        (to - TimeDelta::seconds(1)).format(fmt).to_string()
    };
    format!("{a}..{b}")
}

/// Collapse a line to its shape by replacing variable parts with placeholders,
/// so that repeated events with different IDs and times group together.
/// Each mask leaves one placeholder character (see `config::placeholder`);
/// `display_pattern` spells them out. `#012` is syslog's escaped newline, not
/// a number, so it survives intact.
pub fn normalise(line: &str) -> String {
    let masks = &config::get().masks;
    let pieces: Vec<String> = line
        .trim()
        .split(OCTAL_NL)
        .map(|piece| {
            let mut s = piece.to_string();
            for (i, m) in masks.iter().enumerate() {
                s = m.re.replace_all(&s, config::placeholder(i).to_string().as_str()).into_owned();
            }
            patterns().space.replace_all(&s, " ").into_owned()
        })
        .collect();
    pieces.join(OCTAL_NL).trim().to_string()
}

/// A pattern key as shown: each placeholder becomes its mask's name. Also
/// returns the byte ranges of those names in the result.
pub fn display_pattern(key: &str) -> (String, Vec<std::ops::Range<usize>>) {
    let mut text = String::with_capacity(key.len());
    let mut masks = Vec::new();
    for c in key.chars() {
        match config::mask_of(c) {
            Some(m) => {
                let start = text.len();
                text.push_str(&m.name);
                masks.push(start..text.len());
            }
            None => text.push(c),
        }
    }
    (text, masks)
}

/// How syslog escapes a newline inside one record.
pub const OCTAL_NL: &str = "#012";

/// Parses in chunks across every core: on a 1,000,000-line file this is most of
/// the load time, since reading the lines in is comparatively cheap.
pub fn parse_lines(lines: Vec<String>) -> Vec<Entry> {
    let year = Utc::now().year();
    let n = lines.len();
    if n == 0 {
        return Vec::new();
    }
    let threads = std::thread::available_parallelism().map_or(1, |t| t.get());
    let chunk = n.div_ceil(threads).max(4096);

    let mut rest = lines;
    let mut chunks = Vec::new();
    let mut offset = 0;
    while !rest.is_empty() {
        let take = chunk.min(rest.len());
        let tail = rest.split_off(take);
        chunks.push((offset, rest));
        rest = tail;
        offset += take;
    }

    std::thread::scope(|s| {
        let handles: Vec<_> = chunks
            .into_iter()
            .map(|(offset, chunk)| {
                s.spawn(move || {
                    chunk
                        .into_iter()
                        .enumerate()
                        .filter(|(_, l)| !l.trim().is_empty())
                        .map(|(i, raw)| Entry {
                            line_no: offset + i + 1,
                            file: 0,
                            file_line: offset + i + 1,
                            ts: parse_timestamp(&raw, year),
                            severity: parse_severity(&raw),
                            pattern: normalise(&raw),
                            raw,
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        handles.into_iter().flat_map(|h| h.join().unwrap()).collect()
    })
}

/// Each entry's own timestamp, or the last one before it. Continuation lines
/// such as tracebacks belong to the record that started them.
pub fn carry_forward(entries: &[Entry]) -> Vec<Option<NaiveDateTime>> {
    let mut last = None;
    entries
        .iter()
        .map(|e| {
            if e.ts.is_some() {
                last = e.ts;
            }
            last
        })
        .collect()
}

/// One log file's name, where it was read from (`None` for stdin), and its parsed lines.
pub struct FileLog {
    pub name: String,
    pub path: Option<PathBuf>,
    pub entries: Vec<Entry>,
}

/// Undo `combine_logs`: each file's entries back in their own order.
/// `files` gives the name and path for each `Entry::file` index.
pub fn split_logs(entries: Vec<Entry>, files: &[(String, Option<PathBuf>)]) -> Vec<FileLog> {
    let mut out: Vec<FileLog> = files
        .iter()
        .map(|(name, path)| FileLog { name: name.clone(), path: path.clone(), entries: Vec::new() })
        .collect();
    for e in entries {
        out[e.file].entries.push(e);
    }
    for f in &mut out {
        f.entries.sort_by_key(|e| e.file_line);
    }
    out
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Combined {
    Merged,
    Concatenated,
}

/// Gaps between non-overlapping files wider than this share of the combined
/// span are reported, since the files are probably unrelated or missing time in between.
const GAP_WARNING_FRACTION: f64 = 0.5;

fn span_of(entries: &[Entry]) -> Option<(NaiveDateTime, NaiveDateTime)> {
    entries
        .iter()
        .filter_map(|e| e.ts)
        .fold(None, |range, ts| Some(range.map_or((ts, ts), |(lo, hi): (NaiveDateTime, NaiveDateTime)| (lo.min(ts), hi.max(ts)))))
}

fn renumber(mut entries: Vec<Entry>) -> Vec<Entry> {
    for (i, e) in entries.iter_mut().enumerate() {
        e.line_no = i + 1;
    }
    entries
}

/// Interleave every file's entries by timestamp. A line with no timestamp of
/// its own sorts with the record it continues, as `carry_forward` does within
/// one file; lines before a file's first timestamp sort with that first one.
fn merge_by_time(files: Vec<FileLog>) -> Vec<Entry> {
    let mut keyed: Vec<(NaiveDateTime, Entry)> = Vec::new();
    for f in files {
        let keys = carry_forward(&f.entries);
        let leading = f.entries.iter().find_map(|e| e.ts).unwrap_or(NaiveDateTime::MIN);
        keyed.extend(f.entries.into_iter().zip(keys).map(|(e, k)| (k.unwrap_or(leading), e)));
    }
    keyed.sort_by_key(|(k, _)| *k);
    renumber(keyed.into_iter().map(|(_, e)| e).collect())
}

/// Lay files end to end in order of their span's start. Files with no
/// timestamp at all sort last, in the order they were given.
fn concat_by_time(files: Vec<FileLog>, spans: &[Option<(NaiveDateTime, NaiveDateTime)>]) -> (Vec<Entry>, Option<String>) {
    let mut order: Vec<usize> = (0..files.len()).collect();
    order.sort_by_key(|&i| spans[i].map_or(NaiveDateTime::MAX, |(start, _)| start));

    let dated: Vec<usize> = order.iter().copied().filter(|&i| spans[i].is_some()).collect();
    let warning = (dated.len() >= 2).then(|| {
        let total = (spans[*dated.last().unwrap()].unwrap().1 - spans[dated[0]].unwrap().0).num_seconds().max(1) as f64;
        dated.windows(2).find_map(|w| {
            let gap = (spans[w[1]].unwrap().0 - spans[w[0]].unwrap().1).num_seconds().max(0) as f64;
            let frac = gap / total;
            (frac > GAP_WARNING_FRACTION)
                .then(|| format!("{:.0}% of the span is the gap between {} and {}", frac * 100.0, files[w[0]].name, files[w[1]].name))
        })
    })
    .flatten();

    let mut slots: Vec<Option<FileLog>> = files.into_iter().map(Some).collect();
    let entries = order.into_iter().flat_map(|i| slots[i].take().unwrap().entries).collect();
    (renumber(entries), warning)
}

/// Combine several files' entries into one timeline: merged by timestamp if
/// any two files' time spans overlap, otherwise laid end to end in time order.
pub fn combine_logs(mut files: Vec<FileLog>) -> (Vec<Entry>, Combined, Option<String>) {
    for (i, f) in files.iter_mut().enumerate() {
        f.entries.iter_mut().for_each(|e| e.file = i);
    }    if files.len() <= 1 {
        let entries = files.pop().map_or_else(Vec::new, |f| f.entries);
        return (renumber(entries), Combined::Concatenated, None);
    }

    let spans: Vec<_> = files.iter().map(|f| span_of(&f.entries)).collect();
    let dated: Vec<usize> = (0..files.len()).filter(|&i| spans[i].is_some()).collect();
    let overlaps = dated.len() >= 2 && {
        let mut by_start = dated.clone();
        by_start.sort_by_key(|&i| spans[i].unwrap().0);
        by_start.windows(2).any(|w| spans[w[1]].unwrap().0 <= spans[w[0]].unwrap().1)
    };

    if overlaps {
        (merge_by_time(files), Combined::Merged, None)
    } else {
        let (entries, warning) = concat_by_time(files, &spans);
        (entries, Combined::Concatenated, warning)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_iso_timestamps() {
        let ts = parse_timestamp("2026-10-16T09:21:29 [INFO] up", 2026).unwrap();
        assert_eq!(ts.to_string(), "2026-10-16 09:21:29");
    }

    #[test]
    fn reads_syslog_timestamps() {
        let ts = parse_timestamp("Oct 16 09:21:29 host sshd[11]: x", 2026).unwrap();
        assert_eq!(ts.to_string(), "2026-10-16 09:21:29");
    }

    #[test]
    fn reads_common_log_format() {
        let ts = parse_timestamp(r#"1.2.3.4 - - [16/Oct/2026:09:21:29 +0000] "GET /""#, 2026);
        assert_eq!(ts.unwrap().to_string(), "2026-10-16 09:21:29");
    }

    #[test]
    fn ignores_dates_in_the_payload() {
        assert!(parse_timestamp(&format!("{} 2026-10-16T09:21:29", "x".repeat(80)), 2026).is_none());
    }

    #[test]
    fn ignores_a_date_quoted_inside_a_line() {
        // A systemd status line nested in Ansible output: the date is real, but it
        // is not this line's timestamp.
        let line = "    ActiveEnterTimestamp: Wed 2021-06-23 18:05:10 BST";
        assert!(parse_timestamp(line, 2026).is_none());
    }

    #[test]
    fn still_reads_a_leading_bracketed_timestamp() {
        let ts = parse_timestamp("[2026-10-16 09:21:29] [INFO] up", 2026).unwrap();
        assert_eq!(ts.to_string(), "2026-10-16 09:21:29");
    }

    #[test]
    fn classifies_severity() {
        assert_eq!(parse_severity("a [ERROR] b"), Severity::Error);
        assert_eq!(parse_severity("a WARNING b"), Severity::Warn);
        assert_eq!(parse_severity("a <INFO> b"), Severity::Info);
        assert_eq!(parse_severity("nothing here"), Severity::Other);
    }

    #[test]
    fn severity_needs_a_word_boundary() {
        assert_eq!(parse_severity("terrorism statistics"), Severity::Other);
    }

    #[test]
    fn severity_ignores_hyphenated_identifiers() {
        // A package named libgpg-error is not an error.
        let line = r#"{"name": "libgpg-error", "source": "rpm", "epoch": null}"#;
        assert_eq!(parse_severity(line), Severity::Other);
    }

    #[test]
    fn severity_ignores_quoted_prose() {
        let line = r#"(0, b'ok', b"Warning: Permanently added the host key")"#;
        assert_eq!(parse_severity(line), Severity::Other);
    }

    #[test]
    fn severity_reads_a_leading_level() {
        assert_eq!(parse_severity("ERROR: disk full"), Severity::Error);
    }

    #[test]
    fn http_status_reads_the_code_after_a_quoted_request() {
        let line = r#"127.0.0.1 - - [20/Oct/2026:09:00:00 +0000] "GET /a HTTP/1.1" 500 10"#;
        assert_eq!(http_status(line).unwrap().1, Severity::Error);
        assert_eq!(http_status(r#""POST /b HTTP/1.0" 404 1"#).unwrap().1, Severity::Warn);
        assert_eq!(http_status(r#""GET /c HTTP/1.1" 302 1"#).unwrap().1, Severity::Info);
        assert_eq!(http_status(r#""GET /d HTTP/1.1" 200 1"#).unwrap().1, Severity::Info);
        assert_eq!(http_status(r#""GET /e HTTP/1.1" 101 1"#).unwrap().1, Severity::Debug);
        assert!(http_status("2026-10-16 09:00:00 [ERROR] disk full").is_none());
    }

    #[test]
    fn five_xx_is_another_name_for_error() {
        assert_eq!(Severity::from_name("5xx"), Some(Severity::Error));
        assert_eq!(Severity::from_name("3XX"), Some(Severity::Info));
        assert_eq!(Severity::from_name("error"), Some(Severity::Error));
        assert_eq!(Severity::Error.label_for(LogKind::Web), "5xx");
        assert_eq!(Severity::Info.label_for(LogKind::System), "INFO");
    }

    #[test]
    fn groups_lines_that_differ_only_by_id() {
        let a = normalise("2026-10-16 09:21:29 [ERR] conn 10.0.0.1:443 failed after 30s");
        let b = normalise("2026-10-16 11:02:04 [ERR] conn 10.0.0.9:443 failed after 95s");
        assert_eq!(a, b);
    }

    #[test]
    fn keeps_genuinely_different_lines_apart() {
        let a = normalise("[ERR] disk full on /var");
        let b = normalise("[ERR] connection refused");
        assert_ne!(a, b);
    }

    #[test]
    fn collapses_uuids_and_pids() {
        let a = normalise("sshd[1234]: 550e8400-e29b-41d4-a716-446655440000 ok");
        let b = normalise("sshd[9]: 7d4f1c2a-0000-41d4-a716-446655440000 ok");
        assert_eq!(a, b);
    }

    fn day() -> NaiveDate {
        NaiveDate::from_ymd_opt(2023, 9, 18).unwrap()
    }

    fn range(s: &str) -> (String, String) {
        let (a, b) = parse_range(s, day()).unwrap();
        (a.to_string(), b.to_string())
    }

    #[test]
    fn a_single_time_names_its_whole_minute() {
        assert_eq!(range("15:55"), ("2023-09-18 15:55:00".into(), "2023-09-18 15:56:00".into()));
    }

    #[test]
    fn a_range_end_includes_what_it_names() {
        assert_eq!(range("15:55..15:56"), ("2023-09-18 15:55:00".into(), "2023-09-18 15:57:00".into()));
        assert_eq!(range("15:55:10..15:55:20").1, "2023-09-18 15:55:21");
    }

    #[test]
    fn a_range_can_run_for_a_duration() {
        assert_eq!(range("2023-09-18 15:55..+90s").1, "2023-09-18 15:56:30");
    }

    #[test]
    fn a_time_only_end_rolls_past_midnight() {
        assert_eq!(range("23:50..00:10").1, "2023-09-19 00:11:00");
    }

    #[test]
    fn range_sides_are_optional() {
        let (a, b) = parse_range("..15:00", day()).unwrap();
        assert_eq!(a, NaiveDateTime::MIN);
        assert_eq!(b.to_string(), "2023-09-18 15:01:00");
        assert_eq!(parse_range("15:00..", day()).unwrap().1, NaiveDateTime::MAX);
    }

    #[test]
    fn a_bare_date_or_syslog_day_is_a_whole_day() {
        assert_eq!(range("2023-09-19"), ("2023-09-19 00:00:00".into(), "2023-09-20 00:00:00".into()));
        assert_eq!(range("Feb 15 03:27").0, "2023-02-15 03:27:00");
    }

    #[test]
    fn a_range_round_trips_through_its_text() {
        let r = parse_range("15:55..15:56", day()).unwrap();
        assert_eq!(parse_range(&format_range(r), day()).unwrap(), r);
        let open = parse_range("15:55..", day()).unwrap();
        assert_eq!(parse_range(&format_range(open), day()).unwrap(), open);
    }

    #[test]
    fn rejects_unreadable_or_backwards_ranges() {
        assert!(parse_range("soon", day()).is_err());
        assert!(parse_range("2023-09-18 16:00..2023-09-18 15:00", day()).is_err());
        assert!(parse_range("..+5m", day()).is_err());
    }

    #[test]
    fn severity_token_points_at_the_level() {
        let line = "Feb 15 host app : [WARNING] slow";
        let (r, s) = severity_token(line).unwrap();
        assert_eq!(&line[r], "WARNING");
        assert_eq!(s, Severity::Warn);
    }

    #[test]
    fn skips_blank_lines() {
        let e = parse_lines(vec!["a".into(), "   ".into(), "b".into()]);
        assert_eq!(e.len(), 2);
        assert_eq!(e[1].line_no, 3);
    }

    #[test]
    fn parses_a_chunk_boundary_without_losing_or_misnumbering_lines() {
        // Force several small chunks regardless of how many cores this machine has.
        let lines: Vec<String> = (0..20_000).map(|i| format!("line {i}")).collect();
        let entries = parse_lines(lines);
        assert_eq!(entries.len(), 20_000);
        assert!(entries.windows(2).all(|w| w[1].line_no == w[0].line_no + 1));
        assert_eq!(entries[0].line_no, 1);
        assert_eq!(entries[19_999].line_no, 20_000);
    }

    fn entry(ts: Option<&str>, raw: &str) -> Entry {
        Entry {
            line_no: 0,
            file: 0,
            file_line: 0,
            raw: raw.to_string(),
            ts: ts.map(|s| NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S").unwrap()),
            severity: parse_severity(raw),
            pattern: normalise(raw),
        }
    }

    #[test]
    fn a_single_file_is_renumbered_but_otherwise_untouched() {
        let entries = vec![entry(Some("2026-01-01 00:00:05"), "a"), entry(None, "b")];
        let (out, combined, warning) = combine_logs(vec![FileLog { name: "f".into(), path: None, entries }]);
        assert_eq!(combined, Combined::Concatenated);
        assert!(warning.is_none());
        assert_eq!(out.iter().map(|e| e.raw.as_str()).collect::<Vec<_>>(), ["a", "b"]);
        assert_eq!(out.iter().map(|e| e.line_no).collect::<Vec<_>>(), [1, 2]);
    }

    #[test]
    fn overlapping_files_are_merged_by_timestamp() {
        let a = vec![entry(Some("2026-01-01 00:00:00"), "a1"), entry(Some("2026-01-01 00:00:10"), "a2")];
        let b = vec![entry(Some("2026-01-01 00:00:05"), "b1"), entry(Some("2026-01-01 00:00:15"), "b2")];
        let (out, combined, warning) =
            combine_logs(vec![FileLog { name: "a".into(), path: None, entries: a }, FileLog { name: "b".into(), path: None, entries: b }]);
        assert_eq!(combined, Combined::Merged);
        assert!(warning.is_none());
        assert_eq!(out.iter().map(|e| e.raw.as_str()).collect::<Vec<_>>(), ["a1", "b1", "a2", "b2"]);
        assert_eq!(out.iter().map(|e| e.line_no).collect::<Vec<_>>(), [1, 2, 3, 4]);
        assert_eq!(out.iter().map(|e| e.file).collect::<Vec<_>>(), [0, 1, 0, 1]);
    }

    #[test]
    fn split_logs_undoes_a_merge() {
        let mut a = vec![entry(Some("2026-01-01 00:00:00"), "a1"), entry(Some("2026-01-01 00:00:10"), "a2")];
        let mut b = vec![entry(Some("2026-01-01 00:00:05"), "b1")];
        for (i, e) in a.iter_mut().chain(b.iter_mut()).enumerate() {
            e.file_line = i + 1;
        }
        b[0].file_line = 1;
        let (out, ..) = combine_logs(vec![FileLog { name: "a".into(), path: None, entries: a }, FileLog { name: "b".into(), path: None, entries: b }]);
        let back = split_logs(out, &[("a".into(), None), ("b".into(), None)]);
        let raws = |f: &FileLog| f.entries.iter().map(|e| e.raw.clone()).collect::<Vec<_>>();
        assert_eq!(raws(&back[0]), ["a1", "a2"]);
        assert_eq!(raws(&back[1]), ["b1"]);
    }

    #[test]
    fn normalise_keeps_the_octal_newline() {
        let (text, masks) = display_pattern(&normalise("took 30s#012retry 5 of 9"));
        assert_eq!(text, "took NUMs#012retry NUM of NUM");
        assert_eq!(masks, [5..8, 19..22, 26..29]);
    }

    #[test]
    fn a_masked_value_never_groups_with_the_literal_mask_name() {
        assert_ne!(normalise("from 10.0.0.1"), normalise("from ADDR"));
        assert_eq!(display_pattern(&normalise("from 10.0.0.1:22")).0, "from ADDR");
    }

    #[test]
    fn a_continuation_line_merges_with_the_record_it_follows() {
        let a = vec![entry(Some("2026-01-01 00:00:00"), "a1"), entry(None, "  traceback")];
        let b = vec![entry(Some("2026-01-01 00:00:01"), "b1")];
        let (out, ..) = combine_logs(vec![FileLog { name: "a".into(), path: None, entries: a }, FileLog { name: "b".into(), path: None, entries: b }]);
        assert_eq!(out.iter().map(|e| e.raw.as_str()).collect::<Vec<_>>(), ["a1", "  traceback", "b1"]);
    }

    #[test]
    fn non_overlapping_files_are_concatenated_in_time_order() {
        let a = vec![entry(Some("2026-01-01 00:00:00"), "a1")];
        let b = vec![entry(Some("2026-01-01 00:00:10"), "b1")];
        // Given out of order on the command line.
        let (out, combined, warning) =
            combine_logs(vec![FileLog { name: "b".into(), path: None, entries: b }, FileLog { name: "a".into(), path: None, entries: a }]);
        assert_eq!(combined, Combined::Concatenated);
        assert_eq!(out.iter().map(|e| e.raw.as_str()).collect::<Vec<_>>(), ["a1", "b1"]);
        assert!(warning.is_some(), "a 10s gap is the entire 10s span");
    }

    #[test]
    fn a_small_gap_between_files_does_not_warn() {
        let a = vec![entry(Some("2026-01-01 00:00:00"), "a1"), entry(Some("2026-01-01 00:01:00"), "a2")];
        let b = vec![entry(Some("2026-01-01 00:01:10"), "b1")];
        let (_, combined, warning) =
            combine_logs(vec![FileLog { name: "a".into(), path: None, entries: a }, FileLog { name: "b".into(), path: None, entries: b }]);
        assert_eq!(combined, Combined::Concatenated);
        assert!(warning.is_none(), "a 10s gap in a 70s span is under 50%");
    }

    #[test]
    fn a_file_with_no_timestamps_is_concatenated_last_without_a_gap_warning() {
        let a = vec![entry(Some("2026-01-01 00:00:00"), "a1")];
        let b = vec![entry(None, "b1")];
        let (out, combined, warning) =
            combine_logs(vec![FileLog { name: "b".into(), path: None, entries: b }, FileLog { name: "a".into(), path: None, entries: a }]);
        assert_eq!(combined, Combined::Concatenated);
        assert_eq!(out.iter().map(|e| e.raw.as_str()).collect::<Vec<_>>(), ["a1", "b1"]);
        assert!(warning.is_none());
    }
}
