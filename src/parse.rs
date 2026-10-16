use chrono::{NaiveDate, NaiveDateTime, NaiveTime, Utc, Datelike};
use regex::Regex;
use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Severity {
    Error,
    Warn,
    Info,
    Debug,
    Other,
}

impl Severity {
    pub const ALL: [Severity; 5] = [
        Severity::Error,
        Severity::Warn,
        Severity::Info,
        Severity::Debug,
        Severity::Other,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Severity::Error => "ERROR",
            Severity::Warn => "WARN",
            Severity::Info => "INFO",
            Severity::Debug => "DEBUG",
            Severity::Other => "OTHER",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Entry {
    pub line_no: usize,
    pub raw: String,
    pub ts: Option<NaiveDateTime>,
    pub severity: Severity,
    pub pattern: String,
}

struct Patterns {
    severity: Regex,
    iso: Regex,
    syslog: Regex,
    clf: Regex,
    bare_time: Regex,
    norm: Vec<(Regex, &'static str)>,
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
        norm: vec![
            (
                Regex::new(
                    r"\b[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}\b",
                )
                .unwrap(),
                "<uuid>",
            ),
            (
                Regex::new(r"\b\d{1,3}\.\d{1,3}\.\d{1,3}\.\d{1,3}(:\d+)?\b").unwrap(),
                "<addr>",
            ),
            (
                Regex::new(r"\b\d{4}[-/]\d{2}[-/]\d{2}([T ]\d{2}:\d{2}:\d{2}(\.\d+)?)?\b").unwrap(),
                "<ts>",
            ),
            (Regex::new(r"\b\d{2}:\d{2}:\d{2}(\.\d+)?\b").unwrap(), "<ts>"),
            (Regex::new(r"\b0x[0-9a-fA-F]+\b").unwrap(), "<hex>"),
            (Regex::new(r"\b[0-9a-fA-F]{12,}\b").unwrap(), "<hash>"),
            (Regex::new(r"/[A-Za-z0-9_.\-/]{2,}").unwrap(), "<path>"),
            (Regex::new(r#""[^"]*""#).unwrap(), "<str>"),
            (Regex::new(r"'[^']*'").unwrap(), "<str>"),
            // No trailing \b: numbers are often glued to a unit, as in "30s".
            (Regex::new(r"\b\d+(\.\d+)?").unwrap(), "<n>"),
            (Regex::new(r"\s+").unwrap(), " "),
        ],
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
    match patterns().severity.captures(line).map(|c| c[1].to_string()) {
        Some(m) => match m.to_ascii_uppercase().as_str() {
            "ERR" | "ERROR" | "CRIT" | "CRITICAL" | "FATAL" | "SEVERE" | "PANIC" => Severity::Error,
            "WARN" | "WARNING" => Severity::Warn,
            "INFO" | "NOTICE" => Severity::Info,
            "DEBUG" | "TRACE" => Severity::Debug,
            _ => Severity::Other,
        },
        None => Severity::Other,
    }
}

/// Collapse a line to its shape by replacing variable parts with placeholders,
/// so that repeated events with different IDs and times group together.
pub fn normalise(line: &str) -> String {
    let mut s = line.trim().to_string();
    for (re, rep) in &patterns().norm {
        s = re.replace_all(&s, *rep).into_owned();
    }
    s.trim().to_string()
}

pub fn parse_lines<I: IntoIterator<Item = String>>(lines: I) -> Vec<Entry> {
    let year = Utc::now().year();
    lines
        .into_iter()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty())
        .map(|(i, raw)| Entry {
            line_no: i + 1,
            ts: parse_timestamp(&raw, year),
            severity: parse_severity(&raw),
            pattern: normalise(&raw),
            raw,
        })
        .collect()
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

    #[test]
    fn skips_blank_lines() {
        let e = parse_lines(vec!["a".into(), "   ".into(), "b".into()]);
        assert_eq!(e.len(), 2);
        assert_eq!(e[1].line_no, 3);
    }
}
