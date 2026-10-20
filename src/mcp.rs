//! MCP server over stdio: the same timeline, pattern and line views the TUI
//! shows, for an agent instead of a person. Newline-delimited JSON-RPC 2.0 on
//! stdin and stdout, one request at a time. Every protocol type lives here, so
//! `app.rs` and `parse.rs` stay as free of MCP as they are of ratatui.

use crate::app::{App, Pattern, Query};
use crate::parse::{self, LogKind, Severity};
use crate::{config, expand_globs, open_path, parse_opened};
use chrono::NaiveDateTime;
use regex::Regex;
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

/// Versions of the protocol this speaks. A client asking for one of these gets
/// it back; anything else is answered with the last, and may hang up.
const PROTOCOL_VERSIONS: [&str; 3] = ["2024-11-05", "2025-03-26", "2025-06-18"];

const DEFAULT_SESSION: &str = "default";
/// Rows per page, and the most one call will ever return.
const DEFAULT_LIMIT: usize = 50;
const MAX_LIMIT: usize = 500;
/// A single line is cut to this many characters.
const MAX_TEXT: usize = 500;
/// Backstop on a whole reply, so no tool can flood a context window.
const MAX_REPLY: usize = 64 * 1024;
const DEFAULT_BUCKETS: usize = 60;

// JSON-RPC 2.0 error codes.
const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;

const INSTRUCTIONS: &str = "\
Sawmill cuts up logs. Open files with open_logs, then call overview to see the span, severity mix and the most common line shapes.

Patterns are lines collapsed to a shape: the variable parts (numbers, addresses, UUIDs, paths, quoted strings) are replaced by uppercase mask names such as NUM and ADDR, so lines that differ only by an id group together. overview lists the masks in effect. A group is labelled by the severity most of its lines carry, not by its worst line, so one stray error does not mark hundreds of benign lines.

The usual route: overview for the lie of the land, patterns for the ranked shapes, pattern_lines to read examples behind one shape, context for the raw neighbourhood of an interesting line, timeline to see when things clustered, and set_filters to narrow by level, regex or time before going round again.

Filters are session state and every read reflects them, so call overview if a count surprises you. Results are paged: ask for a small limit and follow next_offset. Regexes are case-insensitive unless you set case_sensitive, and %IP%, %UUID% and any macro from the config file expand inside them.

Timestamps are only read near the start of a line, so dates inside a payload are ignored; a line with no timestamp of its own inherits the one before it, which keeps tracebacks with their record. A session holds the whole log in memory, so close_logs when you are done with it.

An access log is detected on its own. When quoted requests followed by a status outnumber syslog level tokens, lines are classed 5xx, 4xx, 2xx (3xx included), 1xx or OTHER instead of ERROR, WARN, INFO, DEBUG and OTHER. overview's kind field says \"web\" or \"system\". set_filters accepts either set of names.";

/// What a tool failure says to the agent. Protocol faults travel as JSON-RPC
/// errors; a bad argument, a missing file or a broken regex comes back as a
/// normal result with `isError` set, so the agent reads the reason.
type ToolResult = Result<String, String>;

struct RpcError {
    code: i64,
    message: String,
}

fn tools() -> Value {
    let session = json!({
        "type": "string",
        "description": "Session to read, from open_logs. Defaults to \"default\".",
    });
    let offset = json!({ "type": "integer", "minimum": 0, "description": "Rows to skip; use next_offset from the last call." });
    let limit = json!({
        "type": "integer",
        "minimum": 1,
        "description": format!("Rows to return. Defaults to {DEFAULT_LIMIT}, capped at {MAX_LIMIT}."),
    });
    let format = json!({
        "type": "string",
        "enum": ["plain", "json"],
        "description": "plain is one line per row and costs far fewer tokens; json gives n, ts, level, file and text fields.",
    });
    let matching = json!({
        "type": "string",
        "description": "Regex narrowing this call only, on top of the session filters, which are left alone.",
    });

    json!([
        {
            "name": "open_logs",
            "description": "Load log files into a session and describe what arrived: files, line counts, time span, severity mix and the top line shapes. Plain text and gzip are both read, detected by content. Start here.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "paths": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Files to read. Globs and a leading ~/ are expanded; directories and repeats are skipped.",
                    },
                    "session": { "type": "string", "description": "Name for this session. Defaults to \"default\", replacing whatever it held." },
                    "add": { "type": "boolean", "description": "Add to the session rather than replacing it. Files already loaded are skipped." },
                },
                "required": ["paths"],
            },
        },
        {
            "name": "close_logs",
            "description": "Forget a session and free the log it holds.",
            "inputSchema": { "type": "object", "properties": { "session": session } },
        },
        {
            "name": "overview",
            "description": "Where things stand: total and matching line counts, time span, severity mix, the filters in force, the files loaded, the mask legend, and the top line shapes. Cheap; call it whenever a count looks wrong.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "session": session,
                    "top": { "type": "integer", "minimum": 0, "description": "How many of the commonest patterns to include. Defaults to 10." },
                },
            },
        },
        {
            "name": "set_filters",
            "description": "Narrow what every other tool sees. Omitted fields are left alone, null clears one, and calling with no fields at all clears the lot. Returns the new counts.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "session": session,
                    "levels": {
                        "type": "array",
                        "items": { "type": "string", "enum": ["ERROR", "WARN", "INFO", "DEBUG", "OTHER", "5xx", "4xx", "3xx", "2xx", "1xx"] },
                        "description": "Severities to keep. On a web log the same buckets are 5xx, 4xx, 2xx (3xx counts as 2xx), 1xx and OTHER. Either set of names works in either kind.",
                    },
                    "include": { "type": ["string", "null"], "description": "Keep only lines matching this regex." },
                    "exclude": { "type": ["string", "null"], "description": "Drop lines matching this regex." },
                    "time": {
                        "type": ["string", "null"],
                        "description": "Time range: \"15:55\", \"15:55..15:58\", \"15:55..+5m\", \"Feb 15\", \"..15:58\". Either side may be left off. The end includes what it names.",
                    },
                    "case_sensitive": { "type": "boolean", "description": "Regexes are case-insensitive by default." },
                },
            },
        },
        {
            "name": "patterns",
            "description": "Line shapes ranked by how often they occur, with the count and dominant severity of each. The fastest way to see what a log is mostly made of. Use the index with pattern_lines.",
            "inputSchema": {
                "type": "object",
                "properties": { "session": session, "offset": offset, "limit": limit, "matching": matching },
            },
        },
        {
            "name": "pattern_lines",
            "description": "The raw lines behind one shape from patterns. Read a few of these before trusting a shape.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "session": session,
                    "index": { "type": "integer", "minimum": 0, "description": "Index from patterns." },
                    "text": { "type": "string", "description": "Pattern text exactly as patterns reported it, instead of an index." },
                    "offset": offset,
                    "limit": limit,
                    "format": format,
                },
            },
        },
        {
            "name": "lines",
            "description": "The filtered lines in file order. Page with offset, or land somewhere with around_line or at_time.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "session": session,
                    "offset": offset,
                    "limit": limit,
                    "around_line": { "type": "integer", "minimum": 1, "description": "Centre the page on this line number." },
                    "at_time": { "type": "string", "description": "Centre the page on the first line at or after this time, written as for set_filters." },
                    "matching": matching,
                    "format": format,
                },
            },
        },
        {
            "name": "context",
            "description": "The lines either side of one line number, ignoring the filters by default. What to reach for once a suspicious line turns up.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "session": session,
                    "line": { "type": "integer", "minimum": 1, "description": "Line number to centre on, as reported by lines or pattern_lines." },
                    "before": { "type": "integer", "minimum": 0, "description": "Lines before it. Defaults to 5." },
                    "after": { "type": "integer", "minimum": 0, "description": "Lines after it. Defaults to 5." },
                    "respect_filters": { "type": "boolean", "description": "Show only lines passing the session filters. Off by default." },
                    "format": format,
                },
                "required": ["line"],
            },
        },
        {
            "name": "timeline",
            "description": "When the matching lines happened: a severity breakdown per time bucket, with the bucket width chosen to fit the number of buckets asked for. Shows bursts and gaps that line listings hide.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "session": session,
                    "buckets": { "type": "integer", "minimum": 1, "description": format!("How many buckets to divide the span into. Defaults to {DEFAULT_BUCKETS}.") },
                },
            },
        },
    ])
}

pub struct Server {
    sessions: HashMap<String, App>,
}

/// Read requests until stdin closes. `files` are loaded into the default
/// session first, so `sawmill --mcp app.log` is ready to query.
pub fn serve(files: &[PathBuf]) -> io::Result<()> {
    let mut server = Server::new();
    if !files.is_empty() {
        let paths: Vec<String> = files.iter().map(|p| p.display().to_string()).collect();
        let args = json!({ "paths": paths }).as_object().cloned().unwrap_or_default();
        match server.run_tool("open_logs", &args) {
            Ok(_) => eprintln!("sawmill: loaded {} file(s) into the default session", files.len()),
            Err(e) => eprintln!("sawmill: {e}"),
        }
    }

    let stdin = io::stdin().lock();
    let mut out = io::stdout().lock();
    for line in stdin.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if let Some(reply) = server.handle(&line) {
            writeln!(out, "{reply}")?;
            out.flush()?;
        }
    }
    Ok(())
}

impl Server {
    pub fn new() -> Self {
        Server { sessions: HashMap::new() }
    }

    /// One message in, at most one message out. A notification, which has no
    /// `id`, is acted on silently.
    pub fn handle(&mut self, line: &str) -> Option<String> {
        let message: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => return Some(error_reply(Value::Null, PARSE_ERROR, &e.to_string())),
        };
        let id = message.get("id").cloned().unwrap_or(Value::Null);
        let method = message.get("method").and_then(Value::as_str);
        if id.is_null() {
            return None;
        }
        let Some(method) = method else {
            return Some(error_reply(id, INVALID_REQUEST, "no method"));
        };
        let empty = Map::new();
        let params = message.get("params").and_then(Value::as_object).unwrap_or(&empty);
        match self.dispatch(method, params) {
            Ok(result) => Some(result_reply(id, result)),
            Err(e) => Some(error_reply(id, e.code, &e.message)),
        }
    }

    fn dispatch(&mut self, method: &str, params: &Map<String, Value>) -> Result<Value, RpcError> {
        match method {
            "initialize" => Ok(initialize(params)),
            "tools/list" => Ok(json!({ "tools": tools() })),
            "tools/call" => self.call_tool(params),
            "ping" => Ok(json!({})),
            other => Err(RpcError { code: METHOD_NOT_FOUND, message: format!("unknown method {other:?}") }),
        }
    }

    fn call_tool(&mut self, params: &Map<String, Value>) -> Result<Value, RpcError> {
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| RpcError { code: INVALID_PARAMS, message: "tools/call needs a tool name".into() })?
            .to_string();
        let args = params.get("arguments").and_then(Value::as_object).cloned().unwrap_or_default();
        Ok(tool_result(self.run_tool(&name, &args)))
    }

    fn run_tool(&mut self, name: &str, args: &Map<String, Value>) -> ToolResult {
        match name {
            "open_logs" => self.open_logs(args),
            "close_logs" => self.close_logs(args),
            "overview" => self.overview(args),
            "set_filters" => self.set_filters(args),
            "patterns" => self.patterns(args),
            "pattern_lines" => self.pattern_lines(args),
            "lines" => self.lines(args),
            "context" => self.context(args),
            "timeline" => self.timeline(args),
            other => Err(format!("no tool called {other:?}; call tools/list")),
        }
    }

    fn session(&mut self, args: &Map<String, Value>) -> Result<&mut App, String> {
        let id = session_id(args);
        self.sessions
            .get_mut(&id)
            .ok_or_else(|| format!("no session {id:?}: load a log with open_logs first"))
    }

    fn open_logs(&mut self, args: &Map<String, Value>) -> ToolResult {
        let paths = args
            .get("paths")
            .and_then(Value::as_array)
            .ok_or("paths must be an array of files or globs")?;
        let paths: Vec<&str> = paths
            .iter()
            .map(|p| p.as_str().ok_or_else(|| "every path must be a string".to_string()))
            .collect::<Result<_, _>>()?;
        if paths.is_empty() {
            return Err("paths is empty: name at least one file".into());
        }

        let id = session_id(args);
        let add = flag(args, "add")?.unwrap_or(false);
        let existing = self.sessions.get(&id);
        let loaded = match (add, existing) {
            (true, Some(app)) => app.loaded_paths(),
            _ => Vec::new(),
        };

        let (found, mut errors) = expand_globs(paths.iter().copied(), &loaded);
        let counter = Arc::new(AtomicU64::new(0));
        let mut opened = Vec::new();
        for path in &found {
            match open_path(path, &counter) {
                Ok(o) => opened.push(o),
                Err(e) => errors.push(format!("{}: {e}", path.display())),
            }
        }
        if opened.is_empty() {
            return Err(match errors.is_empty() {
                true => "nothing matched those paths".into(),
                false => errors.join("; "),
            });
        }

        let logs = parse_opened(opened);
        let app = match (add, self.sessions.get_mut(&id)) {
            (true, Some(app)) => {
                app.add_logs(logs);
                app
            }
            _ => self.sessions.entry(id.clone()).insert_entry(App::from_logs(logs)).into_mut(),
        };
        if app.entries.is_empty() {
            self.sessions.remove(&id);
            return Err("those files have no lines to show".into());
        }
        if let Some(warning) = app.message.take() {
            errors.push(warning);
        }

        let mut value = overview_value(app, 10);
        value["session"] = json!(id);
        if !errors.is_empty() {
            value["notes"] = json!(errors);
        }
        Ok(to_text(&value))
    }

    fn close_logs(&mut self, args: &Map<String, Value>) -> ToolResult {
        let id = session_id(args);
        match self.sessions.remove(&id) {
            Some(_) => Ok(to_text(&json!({ "closed": id }))),
            None => Err(format!("no session {id:?}")),
        }
    }

    fn overview(&mut self, args: &Map<String, Value>) -> ToolResult {
        let top = number(args, "top")?.unwrap_or(10).min(MAX_LIMIT);
        let app = self.session(args)?;
        Ok(to_text(&overview_value(app, top)))
    }

    fn set_filters(&mut self, args: &Map<String, Value>) -> ToolResult {
        let fields = ["levels", "include", "exclude", "time", "case_sensitive"];
        let touched: Vec<&str> = fields.into_iter().filter(|f| args.contains_key(*f)).collect();
        let app = self.session(args)?;

        if touched.is_empty() {
            app.levels = [true; 5];
            app.include = None;
            app.exclude = None;
            app.time = None;
            if app.case_sensitive {
                app.toggle_case();
            } else {
                app.rebuild();
            }
            return Ok(to_text(&overview_value(app, 0)));
        }

        // Everything is read before anything is applied, so a bad argument
        // leaves the session as it was.
        let levels = args.get("levels").map(levels_from).transpose()?;
        let case = flag(args, "case_sensitive")?;
        let include = args.get("include").map(|v| query(app, "include", v)).transpose()?;
        let exclude = args.get("exclude").map(|v| query(app, "exclude", v)).transpose()?;
        let time = args
            .get("time")
            .map(|v| match v {
                Value::Null => Ok(None),
                Value::String(s) => app.time_range(s),
                _ => Err("time must be a string or null".to_string()),
            })
            .transpose()?;

        if let Some(levels) = levels {
            app.levels = levels;
        }
        if let Some(include) = include {
            app.include = include;
        }
        if let Some(exclude) = exclude {
            app.exclude = exclude;
        }
        if let Some(time) = time {
            app.time = time;
        }
        // `toggle_case` rebuilds the regexes just applied, and the view with them.
        match case {
            Some(want) if want != app.case_sensitive => app.toggle_case(),
            _ => app.rebuild(),
        }
        Ok(to_text(&overview_value(app, 0)))
    }

    fn patterns(&mut self, args: &Map<String, Value>) -> ToolResult {
        let (offset, limit) = page(args)?;
        let app = self.session(args)?;
        let matching = matching(app, args)?;
        let hits: Vec<usize> = (0..app.patterns.len())
            .filter(|&i| matching.as_ref().is_none_or(|re| re.is_match(&app.patterns[i].text)))
            .collect();
        let shown: Vec<Value> = hits
            .iter()
            .skip(offset)
            .take(limit)
            .map(|&i| pattern_value(i, &app.patterns[i], app.kind))
            .collect();
        Ok(paged("patterns", shown, hits.len(), offset))
    }

    fn pattern_lines(&mut self, args: &Map<String, Value>) -> ToolResult {
        let (offset, limit) = page(args)?;
        let plain = plain_format(args)?;
        let index = number(args, "index")?;
        let app = self.session(args)?;
        let pattern = match (index, args.get("text").and_then(Value::as_str)) {
            (Some(i), _) => app
                .patterns
                .get(i)
                .ok_or_else(|| format!("no pattern {i}: there are {}", app.patterns.len()))?,
            (None, Some(text)) => app
                .patterns
                .iter()
                .find(|p| p.text == text || p.key == text)
                .ok_or_else(|| format!("no pattern reads {text:?}; copy one from patterns"))?,
            (None, None) => return Err("give index or text from patterns".into()),
        };
        let total = pattern.lines.len();
        let page: Vec<usize> = pattern.lines.iter().copied().skip(offset).take(limit).collect();
        Ok(render_lines(app, &page, total, offset, plain))
    }

    fn lines(&mut self, args: &Map<String, Value>) -> ToolResult {
        let (mut offset, limit) = page(args)?;
        let plain = plain_format(args)?;
        let around = number(args, "around_line")?;
        let app = self.session(args)?;
        let matching = matching(app, args)?;
        let list: Vec<usize> = app
            .visible
            .iter()
            .copied()
            .filter(|&e| matching.as_ref().is_none_or(|re| re.is_match(&app.entries[e].raw)))
            .collect();

        if let Some(line) = around {
            let at = list.partition_point(|&e| app.entries[e].line_no < line);
            offset = at.saturating_sub(limit / 2);
        } else if let Some(text) = args.get("at_time").and_then(Value::as_str) {
            let (from, _) = app.time_range(text)?.ok_or("at_time is empty")?;
            let at = list.partition_point(|&e| app.time_of(e).is_none_or(|t| t < from));
            offset = at.saturating_sub(limit / 2);
        }

        let page: Vec<usize> = list.iter().copied().skip(offset).take(limit).collect();
        Ok(render_lines(app, &page, list.len(), offset, plain))
    }

    fn context(&mut self, args: &Map<String, Value>) -> ToolResult {
        let line = number(args, "line")?.ok_or("give the line number to centre on")?;
        let before = number(args, "before")?.unwrap_or(5).min(MAX_LIMIT);
        let after = number(args, "after")?.unwrap_or(5).min(MAX_LIMIT);
        let filtered = flag(args, "respect_filters")?.unwrap_or(false);
        let plain = plain_format(args)?;
        let app = self.session(args)?;

        let list: Vec<usize> = match filtered {
            true => app.visible.clone(),
            false => (0..app.entries.len()).collect(),
        };
        let at = list.partition_point(|&e| app.entries[e].line_no < line);
        let from = at.saturating_sub(before);
        let page: Vec<usize> = list.iter().copied().skip(from).take(before + after + 1).collect();
        Ok(render_lines(app, &page, list.len(), from, plain))
    }

    fn timeline(&mut self, args: &Map<String, Value>) -> ToolResult {
        let width = number(args, "buckets")?.unwrap_or(DEFAULT_BUCKETS).clamp(1, MAX_LIMIT);
        let app = self.session(args)?;
        let kind = app.kind;
        let buckets: Vec<Value> = app
            .histogram(width)
            .iter()
            .map(|b| {
                json!({
                    "start": stamp(b.start),
                    "total": b.total(),
                    "counts": severity_map(&b.counts, true, kind),
                })
            })
            .collect();
        Ok(to_text(&json!({
            "bucket_secs": app.bucket_secs,
            "matching": app.visible.len(),
            "buckets": buckets,
        })))
    }
}

fn initialize(params: &Map<String, Value>) -> Value {
    let asked = params.get("protocolVersion").and_then(Value::as_str);
    let version = match asked {
        Some(v) if PROTOCOL_VERSIONS.contains(&v) => v,
        _ => PROTOCOL_VERSIONS[PROTOCOL_VERSIONS.len() - 1],
    };
    json!({
        "protocolVersion": version,
        "capabilities": { "tools": {} },
        "serverInfo": { "name": "sawmill", "version": env!("CARGO_PKG_VERSION") },
        "instructions": INSTRUCTIONS,
    })
}

fn result_reply(id: Value, result: Value) -> String {
    json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string()
}

fn error_reply(id: Value, code: i64, message: &str) -> String {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } }).to_string()
}

fn tool_result(outcome: ToolResult) -> Value {
    let (text, failed) = match outcome {
        Ok(text) => (text, false),
        Err(message) => (message, true),
    };
    json!({ "content": [{ "type": "text", "text": cap(text) }], "isError": failed })
}

/// Trim a reply that got away from us, saying so rather than cutting silently.
fn cap(text: String) -> String {
    if text.len() <= MAX_REPLY {
        return text;
    }
    let mut end = MAX_REPLY;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n[cut here: reply over {MAX_REPLY} bytes, ask for a smaller limit]", &text[..end])
}

fn to_text(value: &Value) -> String {
    value.to_string()
}

fn session_id(args: &Map<String, Value>) -> String {
    args.get("session")
        .and_then(Value::as_str)
        .unwrap_or(DEFAULT_SESSION)
        .to_string()
}

fn number(args: &Map<String, Value>, key: &str) -> Result<Option<usize>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_u64()
            .map(|n| Some(n as usize))
            .ok_or_else(|| format!("{key} must be a whole number, not {v}")),
    }
}

fn flag(args: &Map<String, Value>, key: &str) -> Result<Option<bool>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v.as_bool().map(Some).ok_or_else(|| format!("{key} must be true or false")),
    }
}

fn page(args: &Map<String, Value>) -> Result<(usize, usize), String> {
    let offset = number(args, "offset")?.unwrap_or(0);
    let limit = number(args, "limit")?.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    Ok((offset, limit))
}

fn plain_format(args: &Map<String, Value>) -> Result<bool, String> {
    match args.get("format").and_then(Value::as_str) {
        None | Some("plain") => Ok(true),
        Some("json") => Ok(false),
        Some(other) => Err(format!("format must be \"plain\" or \"json\", not {other:?}")),
    }
}

/// The per-call regex, compiled the way a typed one would be: macros expanded
/// and the session's case rule applied.
fn matching(app: &App, args: &Map<String, Value>) -> Result<Option<Regex>, String> {
    match args.get("matching") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(src)) => Ok(app.compile_query(src)?.map(|q| q.re)),
        Some(_) => Err("matching must be a string".into()),
    }
}

fn query(app: &App, key: &str, value: &Value) -> Result<Option<Query>, String> {
    match value {
        Value::Null => Ok(None),
        Value::String(src) => app.compile_query(src),
        _ => Err(format!("{key} must be a string or null")),
    }
}

fn levels_from(value: &Value) -> Result<[bool; 5], String> {
    let names = value.as_array().ok_or("levels must be an array of level names")?;
    if names.is_empty() {
        return Err("levels is empty: name at least one, or pass null to keep them all".into());
    }
    let mut levels = [false; 5];
    for name in names {
        let name = name.as_str().ok_or("every level must be a string")?;
        let severity = Severity::from_name(name).ok_or_else(|| {
            format!("no level called {name:?}; try ERROR, WARN, INFO, DEBUG, OTHER or 5xx, 4xx, 3xx, 2xx, 1xx")
        })?;
        levels[severity as usize] = true;
    }
    Ok(levels)
}

fn stamp(t: NaiveDateTime) -> String {
    t.format("%Y-%m-%d %H:%M:%S").to_string()
}

/// Level counts by name. `sparse` leaves out the levels with none.
fn severity_map(counts: &[usize; 5], sparse: bool, kind: LogKind) -> Value {
    let map: Map<String, Value> = Severity::ALL
        .iter()
        .filter(|s| !sparse || counts[**s as usize] > 0)
        .map(|s| (s.label_for(kind).to_string(), json!(counts[*s as usize])))
        .collect();
    Value::Object(map)
}

fn truncate(text: &str) -> String {
    match text.char_indices().nth(MAX_TEXT) {
        None => text.to_string(),
        Some((at, _)) => format!("{}\u{2026}", &text[..at]),
    }
}

fn pattern_value(index: usize, p: &Pattern, kind: LogKind) -> Value {
    json!({
        "index": index,
        "count": p.count,
        "severity": p.severity.label_for(kind),
        "text": truncate(&p.text),
    })
}

fn filters_value(app: &App) -> Value {
    json!({
        "levels": Severity::ALL.iter().filter(|s| app.levels[**s as usize]).map(|s| s.label_for(app.kind)).collect::<Vec<_>>(),
        "include": app.include.as_ref().map(|q| q.src.clone()),
        "exclude": app.exclude.as_ref().map(|q| q.src.clone()),
        "time": app.time.map(parse::format_range),
        "case_sensitive": app.case_sensitive,
    })
}

/// What `overview` reports, also returned by `open_logs` and `set_filters` so
/// the effect of a change needs no second call. `top` patterns are listed.
fn overview_value(app: &App, top: usize) -> Value {
    let mut counts = [0; 5];
    let (mut from, mut to) = (None, None);
    for &i in &app.visible {
        let e = &app.entries[i];
        counts[e.severity as usize] += 1;
        if let Some(t) = e.ts {
            from = Some(from.map_or(t, |f: NaiveDateTime| f.min(t)));
            to = Some(to.map_or(t, |l: NaiveDateTime| l.max(t)));
        }
    }

    let mut per_file = vec![0usize; app.files.len()];
    for e in &app.entries {
        if let Some(n) = per_file.get_mut(e.file) {
            *n += 1;
        }
    }
    let files: Vec<Value> = app
        .files
        .iter()
        .zip(&per_file)
        .map(|(name, n)| json!({ "name": name, "lines": n }))
        .collect();

    let mut masks: Vec<&str> = Vec::new();
    for mask in &config::get().masks {
        if !masks.contains(&mask.name.as_str()) {
            masks.push(&mask.name);
        }
    }

    let mut patterns = json!({ "total": app.patterns.len() });
    if top > 0 {
        let shown: Vec<Value> = app
            .patterns
            .iter()
            .enumerate()
            .take(top)
            .map(|(i, p)| pattern_value(i, p, app.kind))
            .collect();
        patterns["top"] = Value::Array(shown);
    }

    json!({
        "source": app.source,
        "files": files,
        "lines": { "total": app.entries.len(), "matching": app.visible.len() },
        "span": match (from, to) {
            (Some(f), Some(t)) => json!({ "from": stamp(f), "to": stamp(t) }),
            _ => Value::Null,
        },
        "kind": match app.kind {
            LogKind::Web => "web",
            LogKind::System => "system",
        },
        "severity": severity_map(&counts, false, app.kind),
        "filters": filters_value(app),
        "patterns": patterns,
        "masks": masks,
    })
}

/// `{ total, offset, next_offset, <key>: [...] }` for a page of a list.
fn paged(key: &str, items: Vec<Value>, total: usize, offset: usize) -> String {
    let shown = items.len();
    let mut out = Map::new();
    out.insert("total".into(), json!(total));
    out.insert("offset".into(), json!(offset));
    if offset + shown < total {
        out.insert("next_offset".into(), json!(offset + shown));
    }
    out.insert(key.into(), Value::Array(items));
    to_text(&Value::Object(out))
}

/// A page of log lines. `plain` renders them as the TUI does, which is much
/// cheaper in tokens than a JSON object per line.
fn render_lines(app: &App, page: &[usize], total: usize, offset: usize, plain: bool) -> String {
    let named = app.files.len() > 1;
    if !plain {
        let items: Vec<Value> = page
            .iter()
            .map(|&i| {
                let e = &app.entries[i];
                let mut line = json!({
                    "n": e.line_no,
                    "level": e.severity.label_for(app.kind),
                    "text": truncate(e.raw.trim_end()),
                });
                if let Some(t) = app.time_of(i) {
                    line["ts"] = json!(stamp(t));
                }
                if named {
                    line["file"] = json!(app.files.get(e.file));
                }
                Value::Object(line.as_object().cloned().unwrap_or_default())
            })
            .collect();
        return paged("lines", items, total, offset);
    }

    let width = page
        .iter()
        .map(|&i| app.entries[i].line_no.to_string().len())
        .max()
        .unwrap_or(1);
    let mut out = match (page.is_empty(), offset + page.len() < total) {
        (true, _) => "no lines here".to_string(),
        (false, true) => format!(
            "lines {}-{} of {total} (next_offset {})\n",
            offset + 1,
            offset + page.len(),
            offset + page.len()
        ),
        (false, false) => format!("lines {}-{} of {total}\n", offset + 1, offset + page.len()),
    };
    for &i in page {
        let e = &app.entries[i];
        out.push_str(&format!("{:>width$} \u{2502} ", e.line_no));
        if named {
            out.push_str(&format!("{} \u{2502} ", app.files.get(e.file).map_or("", String::as_str)));
        }
        out.push_str(&truncate(e.raw.trim_end()));
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOG: &str = "\
2023-09-18 15:54:49 INFO  starting run 1 for host \"alpha\"
2023-09-18 15:54:50 INFO  starting run 2 for host \"beta\"
2023-09-18 15:55:01 WARN  slow reply from 10.0.0.1 after 30s
2023-09-18 15:55:02 ERROR cannot reach 10.0.0.2
    at Thing.connect
2023-09-18 15:56:30 INFO  finished
";

    fn temp_log(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("sawmill-mcp-{}-{label}.log", std::process::id()));
        std::fs::write(&path, LOG).unwrap();
        path
    }

    /// A server with `LOG` open in the default session, and the path to drop.
    fn loaded(label: &str) -> (Server, PathBuf) {
        let path = temp_log(label);
        let mut server = Server::new();
        let out = server.call("open_logs", json!({ "paths": [path.display().to_string()] }));
        assert!(out.is_ok(), "{out:?}");
        (server, path)
    }

    impl Server {
        /// `run_tool` taking the arguments as a literal.
        fn call(&mut self, name: &str, args: Value) -> ToolResult {
            self.run_tool(name, args.as_object().unwrap())
        }

        /// The same, parsed back as JSON.
        fn json(&mut self, name: &str, args: Value) -> Value {
            serde_json::from_str(&self.call(name, args).unwrap()).unwrap()
        }
    }

    fn request(server: &mut Server, line: &str) -> Value {
        serde_json::from_str(&server.handle(line).expect("a request is answered")).unwrap()
    }

    #[test]
    fn a_bad_frame_is_a_parse_error_and_a_notification_is_silent() {
        let mut server = Server::new();
        let reply = request(&mut server, "{not json");
        assert_eq!(reply["error"]["code"], PARSE_ERROR);
        assert_eq!(reply["id"], Value::Null);
        assert_eq!(server.handle(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#), None);
        let reply = request(&mut server, r#"{"jsonrpc":"2.0","id":1,"method":"nope"}"#);
        assert_eq!(reply["error"]["code"], METHOD_NOT_FOUND);
        let reply = request(&mut server, r#"{"jsonrpc":"2.0","id":2}"#);
        assert_eq!(reply["error"]["code"], INVALID_REQUEST);
    }

    #[test]
    fn initialize_echoes_a_known_version_and_offers_the_latest_otherwise() {
        let mut server = Server::new();
        let ask = |server: &mut Server, v: &str| {
            let line = format!(r#"{{"jsonrpc":"2.0","id":1,"method":"initialize","params":{{"protocolVersion":"{v}"}}}}"#);
            request(server, &line)["result"]["protocolVersion"].as_str().unwrap().to_string()
        };
        assert_eq!(ask(&mut server, "2024-11-05"), "2024-11-05");
        assert_eq!(ask(&mut server, "1999-01-01"), *PROTOCOL_VERSIONS.last().unwrap());
        let reply = request(&mut server, r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#);
        assert!(reply["result"]["instructions"].as_str().unwrap().contains("Sawmill cuts up logs"));
        assert_eq!(reply["result"]["serverInfo"]["name"], "sawmill");
    }

    #[test]
    fn tools_list_names_every_tool_it_can_run() {
        let mut server = Server::new();
        let reply = request(&mut server, r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#);
        for tool in reply["result"]["tools"].as_array().unwrap() {
            let name = tool["name"].as_str().unwrap();
            assert!(!tool["description"].as_str().unwrap().is_empty(), "{name} has no description");
            assert_eq!(tool["inputSchema"]["type"], "object", "{name}");
            let out = server.run_tool(name, &Map::new());
            assert_ne!(out, Err(format!("no tool called {name:?}; call tools/list")));
        }
    }

    #[test]
    fn a_failing_tool_answers_with_is_error_not_a_protocol_error() {
        let mut server = Server::new();
        let line = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"overview","arguments":{}}}"#;
        let reply = request(&mut server, line);
        assert!(reply.get("error").is_none(), "a tool failure is a result");
        assert_eq!(reply["result"]["isError"], true);
        let text = reply["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("open_logs"), "{text}");
    }

    #[test]
    fn open_logs_reports_the_span_severities_and_top_patterns() {
        let (mut server, path) = loaded("open");
        let out = server.json("overview", json!({}));
        let _ = std::fs::remove_file(&path);
        assert_eq!(out["lines"], json!({ "total": 6, "matching": 6 }));
        assert_eq!(out["span"]["from"], "2023-09-18 15:54:49");
        assert_eq!(out["span"]["to"], "2023-09-18 15:56:30");
        assert_eq!(out["kind"], "system");
        assert_eq!(out["severity"]["ERROR"], 1);
        assert_eq!(out["severity"]["INFO"], 3);
        assert!(out["masks"].as_array().unwrap().iter().any(|m| m == "NUM"));
        let top = out["patterns"]["top"].as_array().unwrap();
        assert_eq!(top[0]["count"], 2, "the two starting lines share a shape");
        assert_eq!(top[0]["text"], "TS INFO starting run NUM for host STR");
    }

    #[test]
    fn open_logs_without_a_file_fails_and_leaves_no_session() {
        let mut server = Server::new();
        let out = server.call("open_logs", json!({ "paths": ["/no/such/*.log"] }));
        assert!(out.unwrap_err().contains("no such file"));
        assert!(server.sessions.is_empty());
        let out = server.call("open_logs", json!({ "paths": [] }));
        assert!(out.unwrap_err().contains("at least one file"));
    }

    #[test]
    fn sessions_are_separate_and_close_frees_them() {
        let path = temp_log("sessions");
        let mut server = Server::new();
        let paths = json!([path.display().to_string()]);
        server.call("open_logs", json!({ "paths": paths })).unwrap();
        server.call("open_logs", json!({ "paths": paths, "session": "other" })).unwrap();
        server.call("set_filters", json!({ "levels": ["ERROR"] })).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(server.json("overview", json!({}))["lines"]["matching"], 1);
        assert_eq!(server.json("overview", json!({ "session": "other" }))["lines"]["matching"], 6);
        server.call("close_logs", json!({})).unwrap();
        assert!(server.call("overview", json!({})).is_err());
        assert!(server.call("close_logs", json!({})).is_err());
        assert_eq!(server.json("overview", json!({ "session": "other" }))["lines"]["total"], 6);
    }

    #[test]
    fn set_filters_applies_one_field_at_a_time_and_clears_with_no_fields() {
        let (mut server, path) = loaded("filters");
        let matching = |server: &mut Server, args: Value| server.json("set_filters", args)["lines"]["matching"].clone();
        assert_eq!(matching(&mut server, json!({ "levels": ["ERROR", "WARN"] })), 2);
        assert_eq!(matching(&mut server, json!({ "include": "10.0.0.1" })), 1, "levels still apply");
        assert_eq!(matching(&mut server, json!({ "include": null })), 2);
        assert_eq!(matching(&mut server, json!({ "time": "15:55..15:55" })), 2);
        assert_eq!(matching(&mut server, json!({})), 6, "no fields clears everything");
        assert_eq!(matching(&mut server, json!({ "levels": ["5xx"] })), 1, "5xx is ERROR in either kind");
        assert_eq!(matching(&mut server, json!({})), 6);
        let out = server.json("set_filters", json!({ "exclude": "%IP%" }));
        let _ = std::fs::remove_file(&path);
        assert_eq!(out["lines"]["matching"], 4, "macros expand in a filter");
        assert_eq!(out["filters"]["exclude"], "%IP%", "the source is reported, not the expansion");
    }

    #[test]
    fn a_bad_filter_changes_nothing() {
        let (mut server, path) = loaded("badfilter");
        server.call("set_filters", json!({ "levels": ["ERROR"] })).unwrap();
        let errors = [
            server.call("set_filters", json!({ "include": "(unclosed" })).unwrap_err(),
            server.call("set_filters", json!({ "levels": ["LOUD"] })).unwrap_err(),
            server.call("set_filters", json!({ "levels": [] })).unwrap_err(),
            server.call("set_filters", json!({ "case_sensitive": "yes" })).unwrap_err(),
            server.call("set_filters", json!({ "time": "teatime" })).unwrap_err(),
        ];
        let out = server.json("overview", json!({}));
        let _ = std::fs::remove_file(&path);
        assert!(errors[0].contains("bad regex"), "{errors:?}");
        assert!(errors[1].contains("ERROR"), "an unknown level lists the known ones: {errors:?}");
        assert_eq!(out["filters"]["levels"], json!(["ERROR"]), "the session is untouched");
        assert_eq!(out["filters"]["include"], Value::Null);
        assert_eq!(out["lines"]["matching"], 1);
    }

    #[test]
    fn case_sensitivity_carries_into_the_filters_already_set() {
        let (mut server, path) = loaded("case");
        server.call("set_filters", json!({ "include": "WARN" })).unwrap();
        assert_eq!(server.json("overview", json!({}))["lines"]["matching"], 1);
        server.call("set_filters", json!({ "include": "warn" })).unwrap();
        assert_eq!(server.json("overview", json!({}))["lines"]["matching"], 1, "insensitive by default");
        let out = server.json("set_filters", json!({ "case_sensitive": true }));
        let _ = std::fs::remove_file(&path);
        assert_eq!(out["lines"]["matching"], 0, "the standing include is recompiled");
        assert_eq!(out["filters"]["case_sensitive"], true);
    }

    #[test]
    fn patterns_page_and_narrow_without_touching_the_filters() {
        let (mut server, path) = loaded("patterns");
        let all = server.json("patterns", json!({}));
        let first = server.json("patterns", json!({ "limit": 1 }));
        let next = server.json("patterns", json!({ "limit": 1, "offset": 1 }));
        let narrow = server.json("patterns", json!({ "matching": "cannot reach" }));
        let out = server.json("overview", json!({}));
        let _ = std::fs::remove_file(&path);
        assert_eq!(first["total"], all["total"]);
        assert_eq!(first["next_offset"], 1);
        assert_eq!(first["patterns"][0]["count"], 2);
        assert_eq!(next["offset"], 1);
        assert_ne!(next["patterns"][0]["text"], first["patterns"][0]["text"]);
        assert_eq!(narrow["total"], 1);
        assert_eq!(narrow["patterns"][0]["severity"], "ERROR");
        assert!(narrow.get("next_offset").is_none(), "a complete page has no next");
        assert_eq!(out["lines"]["matching"], 6, "matching is per call, not session state");
    }

    #[test]
    fn pattern_lines_take_an_index_or_the_text_and_report_the_raw_lines() {
        let (mut server, path) = loaded("drill");
        let by_index = server.call("pattern_lines", json!({ "index": 0 })).unwrap();
        let text = server.json("patterns", json!({ "limit": 1 }))["patterns"][0]["text"].clone();
        let by_text = server.call("pattern_lines", json!({ "text": text })).unwrap();
        let missing = server.call("pattern_lines", json!({ "index": 99 })).unwrap_err();
        let neither = server.call("pattern_lines", json!({})).unwrap_err();
        let _ = std::fs::remove_file(&path);
        assert_eq!(by_index, by_text);
        assert!(by_index.contains("starting run 1 for host \"alpha\""), "{by_index}");
        assert!(by_index.contains("starting run 2 for host \"beta\""));
        assert!(missing.contains("no pattern 99"), "{missing}");
        assert!(neither.contains("index or text"), "{neither}");
    }

    #[test]
    fn lines_land_on_a_line_number_or_a_time() {
        let (mut server, path) = loaded("lines");
        let plain = server.call("lines", json!({ "limit": 2 })).unwrap();
        let around = server.call("lines", json!({ "around_line": 5, "limit": 2 })).unwrap();
        let at = server.call("lines", json!({ "at_time": "15:55:02", "limit": 1 })).unwrap();
        let _ = std::fs::remove_file(&path);
        assert!(plain.starts_with("lines 1-2 of 6 (next_offset 2)\n"), "{plain}");
        assert!(around.contains("at Thing.connect"), "{around}");
        assert!(around.starts_with("lines 4-5 of 6"), "centred on line 5: {around}");
        assert!(at.contains("cannot reach"), "{at}");
    }

    #[test]
    fn json_format_carries_the_inherited_timestamp_of_a_continuation_line() {
        let (mut server, path) = loaded("json");
        let out = server.json("lines", json!({ "around_line": 5, "limit": 1, "format": "json" }));
        let _ = std::fs::remove_file(&path);
        let line = &out["lines"][0];
        assert_eq!(line["n"], 5);
        assert_eq!(line["text"], "    at Thing.connect");
        assert_eq!(line["ts"], "2023-09-18 15:55:02", "it inherits the record above");
        assert!(line.get("file").is_none(), "one file needs no file column");
    }

    #[test]
    fn context_ignores_the_filters_unless_told_otherwise() {
        let (mut server, path) = loaded("context");
        server.call("set_filters", json!({ "levels": ["ERROR"] })).unwrap();
        let raw = server.call("context", json!({ "line": 4, "before": 1, "after": 1 })).unwrap();
        let filtered = server.call("context", json!({ "line": 4, "respect_filters": true })).unwrap();
        let missing = server.call("context", json!({})).unwrap_err();
        let _ = std::fs::remove_file(&path);
        assert!(raw.contains("slow reply"), "the line before is filtered out but shown: {raw}");
        assert!(raw.contains("at Thing.connect"));
        assert_eq!(filtered.lines().count(), 2, "one header and the one matching line: {filtered}");
        assert!(missing.contains("line number"), "{missing}");
    }

    #[test]
    fn timeline_buckets_the_matching_lines_by_severity() {
        let (mut server, path) = loaded("timeline");
        let out = server.json("timeline", json!({ "buckets": 20 }));
        let _ = std::fs::remove_file(&path);
        let buckets = out["buckets"].as_array().unwrap();
        let counted: i64 = buckets.iter().map(|b| b["total"].as_i64().unwrap()).sum();
        assert_eq!(counted, 5, "the continuation line has no stamp of its own");
        assert!(out["bucket_secs"].as_i64().unwrap() > 0);
        assert_eq!(buckets[0]["start"], "2023-09-18 15:54:49", "buckets run from the first stamp");
        assert_eq!(buckets[0]["counts"]["INFO"], 2);
        assert!(buckets[0]["counts"].get("DEBUG").is_none(), "empty levels are left out: {buckets:?}");
    }

    #[test]
    fn a_limit_is_capped_and_a_silly_argument_is_named() {
        let (mut server, path) = loaded("limits");
        let out = server.json("patterns", json!({ "limit": 10_000 }));
        let bad = server.call("lines", json!({ "limit": -1 })).unwrap_err();
        let format = server.call("lines", json!({ "format": "yaml" })).unwrap_err();
        let _ = std::fs::remove_file(&path);
        assert!(out["patterns"].as_array().unwrap().len() <= MAX_LIMIT);
        assert!(bad.contains("limit must be a whole number"), "{bad}");
        assert!(format.contains("plain"), "{format}");
    }

    #[test]
    fn a_long_line_is_cut_and_so_is_a_runaway_reply() {
        assert_eq!(truncate("short"), "short");
        let long = "x".repeat(MAX_TEXT * 2);
        let cut = truncate(&long);
        assert_eq!(cut.chars().count(), MAX_TEXT + 1);
        assert!(cut.ends_with('\u{2026}'));
        assert_eq!(cap("fine".to_string()), "fine");
        let flood = cap("\u{00e9}".repeat(MAX_REPLY));
        assert!(flood.ends_with("ask for a smaller limit]"), "{}", &flood[flood.len() - 60..]);
        assert!(flood.len() < MAX_REPLY + 100);
    }
}
