---
name: sawmill
description: >-
  Triage logs with Sawmill, a terminal app and MCP server that groups lines
  into patterns, filters by severity and time, and draws a timeline. Use when
  reading, filtering, summarizing, or investigating log files, gzip logs, or
  piped command output, or when calling sawmill --mcp (open_logs, overview,
  patterns, pattern_lines, lines, context, timeline, set_filters).
---

# Sawmill

Sawmill dissects logs. One binary, two fronts: a terminal UI, and `--mcp` for an agent. Both apply the same parse, the same masks, and the same filters.

`README.md` is the human overview. This file is how an agent uses the product.

## Use the tools first

For pattern frequency, severity mix, a time window, or what a log is mostly made of, use Sawmill. Start with the MCP tools below. Use grep, awk, or jq only for a question those tools cannot express.

## Triage

1. `open_logs` loads the files into a named session and returns an overview.
2. `overview` is the lie of the land: kind, span, severity mix, filters, files, mask names, and the top shapes. Call it again whenever a count looks wrong.
3. `patterns` ranks shapes by count. Read the mask names as placeholders.
4. `pattern_lines` shows the raw lines behind one shape. Read a few before trusting the shape.
5. `context` shows the lines around one line number.
6. `set_filters` narrows every later read. Then `overview` and `timeline` to check the hypothesis.
7. `lines` pages the filtered stream, or lands on `around_line` / `at_time`.
8. `close_logs` frees the session. A large log is a lot of memory.

`open_logs` and `set_filters` already include an overview, so a change needs no second call unless you want a fresh `top`.

### Sessions and transport

`session` defaults to `"default"`. Files named on the command line are loaded there before the first request. `open_logs` replaces that session; `add: true` appends, and files already loaded are skipped. Paths accept globs and a leading `~/`. Directories and repeats are skipped.

Stdin is the protocol. It is never read as a log. Diagnostics go to stderr.

One JSON-RPC request is answered before the next is read. A slow `open_logs` blocks the client. Methods are `initialize`, `tools/list`, `tools/call`, and `ping`. A notification is acted on and not answered. Protocol versions `2024-11-05`, `2025-03-26`, and `2025-06-18` are accepted and echoed; anything else is answered as the newest.

A bad frame or an unknown method is a JSON-RPC error. A mistake an agent can make — missing file, missing session, bad regex, unknown level — comes back as a normal result with `isError: true` and the reason as text. A bad `set_filters` changes nothing.

### Pages and cost

`limit` defaults to 50 and caps at 500. Lists report `total`, `offset`, and `next_offset`. Each line is cut at 500 characters. A reply is cut at 64KB and says so.

Plain text is the default (`N │ text`, plus a file column when more than one file is loaded). `format: "json"` adds `n`, `ts`, `level`, `file`, and `text`. `patterns` and `lines` take `matching`, a regex for that call only; session filters stay as they are.

`context` ignores session filters unless `respect_filters` is set. `before` and `after` default to 5. `timeline` divides the span into `buckets` (default 60).

```json
{
  "mcpServers": {
    "sawmill": {
      "command": "sawmill",
      "args": ["--mcp"]
    }
  }
}
```

`sawmill --mcp app.log` is the same server with `default` already loaded.

### `set_filters`

Omitted fields stay. `null` clears one field. A call with no fields clears them all. `levels` takes `ERROR`, `WARN`, `INFO`, `DEBUG`, `OTHER`, or the web names `5xx`, `4xx`, `3xx`, `2xx`, `1xx`. Either set works on either kind of log. `3xx` is the same bucket as `2xx`.

`time` takes `15:55`, `15:55..15:58`, `15:55..+5m`, `Feb 15`, `..15:58`, with either side optional. The end includes the moment it names.

## How to read a result

### Patterns

A pattern is the line with its variable parts replaced by mask names from the config, written as they appear: `IPv4`, `IPv6`, `NUM`, `DATE`, `UUID`, and so on. `overview` lists the masks in effect. Lines that differ only by an id, address, or number land in one group.

The name is a placeholder for the match. The same letters as ordinary text are a different pattern. Pass `pattern_lines` the index from `patterns`, or the text exactly as it was printed.

A group is labelled by the severity most of its lines carry. A tie goes to the more severe level. One error inside a quiet pattern leaves the label alone. A real level token survives masking, so `[INFO]` and `[ERROR]` are already different groups.

### Time

A timestamp counts when it begins within 4 characters of the start of the line. Common Log Format is the exception: the client address comes first, and `[dd/Mon/yyyy:` may begin within 64 characters. A date later in the line is payload. It does not move the histogram.

The shapes are ISO (`2026-10-16`, `2026-10-16T09:21:29.123Z`, `2026/10/16 09:21:29+00:00`), a month name (`Oct 16 09:21:29`, `October 16, 2026`), Common Log (`16/Oct/2026:09:21:29`), day then month (`16 October 2026`), and numeric (`16/10/2026`, `10-16-26`). A date with no clock is midnight. A numeric date is day-first when both numbers could be a month, and month-first when only the second is greater than 12. A two-digit year pivots at 70. The zone offset is dropped and the clock is kept as written.

A line with no timestamp of its own inherits the previous one for filtering, so a traceback stays with its record. The histogram counts only lines that carry a timestamp of their own.

A syslog date with no year uses the current year.

### Severity and kind

A level is a delimited token. `libgpg-error` is not an error, and `"Warning: …"` inside quotes is prose.

`overview` reports `kind`. `web` means quoted requests followed by a status (`"GET /path HTTP/1.1" 200`) outnumber those level tokens, and there are at least three statuses. Otherwise the log is `system`.

On a web log the five buckets are `5xx`, `4xx`, `2xx` (with `3xx`), `1xx`, and other (no status). The terminal status bar shows `5 4 3 2 1 0`; `3` and `2` are one filter.

### Line numbers

Blank lines are omitted. In a single file the reported number is the line in that file, so a blank leaves a gap in the sequence. With more than one file the number is the combined sequence, and the file column names the source. That combined number is what `around_line`, `context`, and the terminal gutter use.

### Several files

Each file is parsed, then joined. Overlapping spans are interleaved by timestamp. Spans that do not overlap are laid end to end by when they start, which may differ from command-line order. A file with no timestamps sorts last, in the order given. A gap wider than half the combined span is named in the terminal status bar.

An empty pipe beside real files is ignored. Stdin cannot be removed from the terminal file list.

## Filters and macros

Regexes are case-insensitive unless `case_sensitive` is set. Inside any regex, `%NAME%` expands to a macro from the config. Built in, unless the config defines the same name: `%IP4%` (an IPv4 address with an optional port), `%IP6%`, and `%DATE%` (the five timestamp shapes as one alternation). The default config also defines `%UUID%` and `%OBJECT%`. In the terminal, `%FIND%` expands to the current find term.

Find highlights and jumps. Include and exclude decide which lines exist.

## Terminal

Press `?` for the key list.

| Key | Action |
|---|---|
| `e` `w` `i` `d` `o` | Toggle Error, Warn, Info, Debug, Other |
| `E` `W` `I` `D` `O` | That level only; again for all |
| `5` `4` `3` `2` `1` `0` | The same toggles on a web log |
| `/` `\` | Include / exclude regex. Empty clears it |
| `Tab` | Cycle Include, Exclude, Find, then close |
| `c` | Case sensitivity |
| `t` `T` | Type a time range. Empty or `T` clears it |
| `f` `F` `P` `N` | Find, clear find, previous hit, next hit |
| `g` | Go to a line number |
| `a` | All lines, in file order |
| `Enter` | Pattern → its lines → that line in context → its pattern |
| `l` | Wrap the cursor row. Other rows stay truncated |
| `←` `→` | Scroll sideways when wrap is off |
| `s` | Mouse off, so the terminal can copy |
| `Space` | Bookmark the cursor line, in Lines and All lines |
| `p` `n` | Previous / next bookmark |
| `B` | Clear bookmarks, after `y` / `n` |
| `q` | Quit, after `y`, `q`, or Enter. Ctrl-C quits at once |
| `Esc` | Clear find, then step back. Backspace pops a mouse time zoom first |

`Esc` on the pattern view offers Files, Settings, Themes, then Quit. Files adds globs, removes a loaded file when more than one is loaded, and clears filters. Settings toggles wrap, mouse, case sensitivity, and power scroll, and can edit include and exclude. Those four toggles and the theme are written back to `[ui]` only. Themes marks the current palette with `*`. `terminal` keeps the terminal's own colours and is the fallback for an unknown name.

Bookmarks last for the session. A marked line has a bright green mark before its number, and a bucket that holds one has a mark above its bar. Right-click in Lines or All lines offers Bookmark or Remove bookmark for that row.

Drag on the timeline sets the time range. A double-click keeps the five buckets centred on the click, clipped at the ends. A single click on the pattern view does nothing. In Lines and All lines, a single click jumps to that bucket. Mouse time changes stack; Backspace pops one. Typing a range, `T`, an empty `t`, or Clear time filter empties the stack. Right-click on the timeline offers Clear time filter and Choose time period. Hover shows the bucket's span and count.

Drag inside a row selects within that row. Double-click takes a word (`[A-Za-z0-9_-]+`); triple-click takes the text up to whitespace. Right-click on the text offers Include, Exclude, or Find, and the selection replaces that filter as an escaped regex. Double-click a pattern's count or level to drill in. Double-click a raw line's number or file name to open it in All lines.

Wrap turns `#012` into a line break on the cursor row, and the block stops at 75% of the list height. Everywhere else `#012` is drawn as `⏎`, and it stays a break during grouping. The cursor row is bold on grey, so find hits keep their colour.

Esc during loading exits 130. Gzip progress is compressed bytes read against the file size.

## Config

`~/.config/sawmill/config.toml`, or `$XDG_CONFIG_HOME/sawmill/config.toml`. The file is written on first run from the built-in defaults.

| Section | Role |
|---|---|
| `[[mask]]` | Name plus regex, or `builtin` of `IP4`, `IP6`, or `DATE`. They run in order |
| `[macros]` | `NAME = 'regex'` becomes `%NAME%`. A name that matches a builtin replaces it |
| `[[highlight]]` | Colour for raw lines and pattern text. Later rules paint over earlier ones |
| `[ui]` | `theme`, `wrap`, `mouse`, `case_sensitive`, `power_scroll` |

A section left out keeps its default. A section that is present replaces that default. A key left out of `[ui]` keeps its default. A file that does not parse prints the error and exits 2.

Mask order is the match. The built-in order puts UUID, then IPv6, then IPv4, then the date shapes, then a bare clock (`TIME`), hex, hashes, and quoted strings, and `NUM` last. `NUM` has no trailing boundary, so `30s` and `95s` collapse together. IPv6 runs before IPv4 so an IPv4-mapped address stays one token. `builtin = "IP4"` has no leading word boundary. A bare clock on its own is `TIME`; it is also a timestamp when it sits at the start of the line.

A colour name follows the current theme. `#rrggbb` and a 256-colour index stay as written. Mask names are drawn struck through on top of the highlight rules.

## Run

The release binary targets Linux amd64. Elsewhere, build it.

```bash
sawmill app.log
sawmill app.log.gz
sawmill app1.log app2.log
some-command 2>&1 | sawmill
some-command 2>&1 | sawmill app.log
sawmill --mcp
```

```bash
cargo run --release -- app.log
cargo run --release -- --mcp
```

Gzip is detected from the file, not the name. With neither a file nor a pipe, Sawmill exits 2. An empty file, or a pipe on its own with no lines, exits 1.

## Limits

- There is no switch that keeps only lines with their own timestamp. Payload and continuation lines are real patterns. On a log that is mostly structured payload, the top shapes are those fragments. Narrow them with include, exclude, or a time range.
- The file is loaded whole. There is no follow mode.
- A selection stays inside one row. Columns are one cell wide, so a selection on a wide character can miss.
- Histogram columns are one cell wide. Dragging and `t` cover them; there is no keyboard bucket cursor.
- A line that is not valid UTF-8 ends the read of that file.
- MCP serves tools only: no resources, prompts, sampling, progress notifications, or batched requests.
