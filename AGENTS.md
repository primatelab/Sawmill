# Sawmill — agent handoff

Interactive log triage TUI ("Dissecting Logs"). Standalone project at `~/Projects/Sawmill`. It was Logsview until 2026-10-19. Gordon chose this over putting it under `~/Projects/SkunkWorks`. He does not have time to work on it right now; pick up from here.

Commit `d6296a8` on `master` is local only. No remote. Do not commit or push unless he asks. `AGENTS.md` itself is uncommitted.

## What it is

A ratatui app that loads a log and shows three things:

1. **Timeline** — stacked severity histogram. Bucket width snaps to a ladder (`1s` … `1d`) so the span fits the terminal width.
2. **Patterns** — lines collapsed to a shape (UUIDs, addresses, paths, hex, quoted strings, numbers become placeholders) and ranked by frequency. This is the `loguniq` idea, generalised past stripping a fixed timestamp column.
3. **Lines** — `Enter` drills into the raw lines behind the selected pattern.

A fourth view, **All lines** (`a`), is the filtered stream in file order, LogSnout-style. `Enter` walks pattern → its lines → that line in context → its pattern.

LogSnout (`~/Code/LogSnout/logsnout.sh`) features were ported on 2026-10-16. `?` shows the key list in the app; the source of truth is `HELP` in `ui.rs` and `on_key` in `main.rs`. In short: `e w i d o` toggle a level and `E W I D O` solo it (again for all); on a web log, `5 4 3 2 1 0` do the same toggles (`3` and `2` are one filter, `0` is other) and the letter keys still work; `Space`/`Tab` include/exclude regex; a prompt edits with the cursor (`tui-input`: arrows, Home/End, Ctrl-arrows by word, Ctrl-Backspace/Ctrl-Delete delete a word); `c` case; `f` find with `n`/`N`; `g` go to line; `l` wrap; `←`/`→` sideways; `p` power scroll; `s` mouse off for copying; `Esc` clears find, then goes back; on the pattern view it offers Files, Settings, Themes or Quit (below). Macros from the config (`%IP%`, `%UUID%` by default) and the built-in `%FIND%` expand inside any regex. Regexes are case-insensitive by default, as in LogSnout. `f` used to cycle severity; it is find now.

Time filtering: `t` takes `15:55`, `15:55..15:58`, `15:55..+5m`, `Feb 15`, either side optional; or drag across the timeline. The end is inclusive of what it names. Lines without a timestamp inherit the previous one for time filtering only, so tracebacks stay with their record. The histogram itself still counts only stamped lines.

Mouse and quit (added 2026-10-16): `q` asks "Are you sure?" (`y`/`q`/`Enter` quits, anything else cancels; Ctrl-C quits at once). Hovering the timeline shows the bucket's time span and line count. In the raw-line views (Lines and All lines) the bucket holding the cursor line (`App::cursor_time`, carried forward for continuation lines) is drawn in lighter colours on a grey background, the same as a drag in progress (`Theme::severity_lit` and the `lit` predicate in `render_bars`). Clicking a bucket in those views jumps the cursor to its first line, or the nearest line by time (`App::release_timeline`); a drag still sets the time filter, and on the pattern view a click does too. Right-clicking the timeline offers Clear time filter and Choose time period. In the list, drag selects within one item (leaving the item clamps to its start or end), double-click on the text takes `[alnum-_]+`, triple-click takes everything up to whitespace, a plain click only moves the cursor. Double-clicking a pattern's count or level drills in, as Enter does. Double-clicking the line number or file name of a raw line opens it in the all-lines view, again as Enter does. A `│` separates that gutter from the text, and the line number from a raw line. Right-click on the text opens Include / Exclude / Find; the text is `regex::escape`d and replaces the existing filter. Lists draw a vertical scrollbar when they overflow and a horizontal one when a line is wider than the view and wrap is off. Both drag: a press on the track jumps there (`App::vbar`/`hbar` record where they were drawn), the arrows step by one. Wrap (`l`) applies to the cursor row only, in both the pattern and raw-line views; every other row is truncated, and horizontal scroll is ignored while wrap is on. With wrap on, `#012` is a newline, and the wrapped block stops at 75% of the list height (`wrap_rows` in `ui.rs`) with a `…`. On every row that is not wrapped, `#012` is drawn as `⏎`. `normalise` splits on `#012` so it survives grouping instead of becoming a number placeholder. The cursor row is drawn bold on a grey background (`cursor_style` in `ui.rs`), not reversed, so find hits and selections keep their colours on it. Raw-line rows show the file name after the line number (`Entry::file` indexes `App::files`, set by `combine_logs`; names are cut at 24 characters). `ui.rs` records where each list row's text was drawn (`App::rows`, `RowHit`) so mouse columns map back to characters; a selection is keyed to view, pattern and filter generation and dies when any changes. Loading runs on a thread behind a gauge; for gzip the percentage is compressed bytes read against file size. Above the gauge, five frames from `~/Code/buzz` (snapshotted in `src/buzz.rs`) play at 10fps. Esc during loading aborts with exit 130.

Not ported from LogSnout: tabulate (`b`), the `~/.config/loggs` config file and `conf` editor, and syslog hostname tinting.

Config (added 2026-10-19): `~/.config/sawmill/config.toml` (or `$XDG_CONFIG_HOME/sawmill/`) holds `[[mask]]` (name + regex, run in order), `[macros]` (`NAME = 'regex'` gives `%NAME%`), `[[highlight]]` (regex, `fg`/`bg` as ratatui colour names, `#rrggbb` or an index, plus `bold`/`italic`/`dim`/`underline`) and `[ui]` (theme, wrap, mouse, case_sensitive, power_scroll). `config::DEFAULT` is both the built-in rules and what a first run writes when the file is missing. As of 2026-10-20 that string is Gordon's config: the `PATH` mask is commented out, `ADDR` has no leading word boundary, and the highlight rules include braces, Python `#012 File`, `::` modules and `~*/` paths. A section left out falls back to its default; a section present replaces it entirely, except `[ui]`, where a missing key keeps its default. A bad file prints the error and exits 2. `config::get()` returns the defaults when `init` was never called, so tests never read the user's file. Highlight rules colour raw lines and pattern text; a mask name is drawn struck through on top of them. A colour name is mapped onto the current theme when it is drawn (`Theme::resolve`); `#rrggbb` and a 256-colour index stay as written. The terminal theme leaves names as ANSI colours. Changing the theme or a Settings toggle rewrites only the `[ui]` block (`config::replace_ui`), so comments and the other sections stay. Unit tests do not write the file.

Masks: each mask leaves one private-use character (`U+E000 + index`) in `Entry::pattern`, so a masked address and the literal word `ADDR` never group together. `parse::display_pattern` turns that key into `Pattern::text` (mask names in uppercase, e.g. `ADDR`, `NUM`) plus `Pattern::masks` byte ranges, which `highlight::pattern` strikes through. Compare patterns by `Pattern::key`, never by `text`.

Esc on the pattern view offers Files, Settings, Themes, then Quit. Quit asks the same "Are you sure?" as `q`. Settings toggles line wrap, mouse, case sensitivity and power scroll (the menu stays open and the label flips between on and off), opens the include and exclude prompts, and clears filters. The same four toggles on the keyboard (`l`, `s`, `c`, `p`) save too. Themes lists Herdr's eighteen themes (catppuccin through vesper, including `terminal`) and then Monokai, from the original palette; the current one is marked `*` and the menu stays open. `terminal` is the original ANSI look, and the one an unknown name falls back to. A truecolor theme fills the frame; the buzzsaw frames stay as they were drawn. Files (added 2026-10-19) has Add files… (a prompt taking whitespace-separated globs, `~/` expanded; directories, already-loaded files and repeats are skipped; misses are reported), Remove <name> for each file when more than one is loaded, and Clear filters when any are set. Stdin cannot be removed; when it contributed any lines it is listed in place, drawn grey, and the cursor, mouse and initial-letter shortcut all skip it. `App::pending_add` hands the globs to `main`, which loads them behind the same gauge as startup (`load()`; Esc cancels the add instead of exiting). Adding or removing splits the entries back per file (`parse::split_logs`, ordered by `Entry::file_line`, the per-file line number), then recombines everything through `App::set_logs`, so merge vs. concatenate and the gap warning are decided again. Filters and toggles stay; cursors reset.

```bash
cargo run -- ~/Code/ansible-testlog.log
cargo run --release -- ~/million.log.gz   # gzip detected by magic bytes
cargo run --release -- app1.log app2.log  # merged or concatenated, see below
some-command 2>&1 | cargo run
some-command 2>&1 | cargo run -- app.log   # the pipe and the file, combined
cargo run --release -- --mcp ~/Code/ansible-testlog.log   # MCP server, see below
```

A terminal with neither a file nor a pipe exits 2. A pipe is read as `stdin` as well as any named files; an empty pipe beside files is ignored. An empty file, or a pipe on its own with no lines, exits 1.

Multiple files (added 2026-10-16) are each parsed, then `parse::combine_logs` decides how to join them: if any two files' time spans overlap, every entry is interleaved by timestamp (`Combined::Merged`); otherwise the files are laid end to end in order of their span's start, not command-line order (`Combined::Concatenated`). A continuation line with no timestamp of its own carries the one before it, same as `App`'s own `when` vector — both now share `parse::carry_forward`. `line_no` is renumbered 1.. over the combined order; the per-file number is kept in `Entry::file_line` so files can be split apart again. If two concatenated files' gap is more than half the combined span, `App::message` carries a one-line warning naming them, shown in the status bar like any other message. A file with no timestamps at all can't be placed, so it sorts last, in the order given, and is excluded from the gap check. Stdin and a lone file are unaffected: `Combined::Concatenated` with no files to warn about.

## MCP mode

`--mcp` (added 2026-10-19) serves the same views to an agent instead of a person: a hand-rolled JSON-RPC 2.0 server, newline-delimited on stdin and stdout, one request at a time, no SDK and no async. `serde_json` is the only dependency it added. The `rmcp` crate was turned down because it drags in tokio.

Nine tools: `open_logs`, `close_logs`, `overview`, `set_filters`, `patterns`, `pattern_lines`, `lines`, `context`, `timeline`. `initialize`, `tools/list`, `tools/call` and `ping` are the only methods; a notification is acted on and not answered. Protocol versions `2024-11-05`, `2025-03-26` and `2025-06-18` are accepted and echoed, anything else gets the newest. The `instructions` string in the initialize result is what teaches an agent about masks and the triage route, so change it when the tools change.

A protocol fault (bad frame, unknown method) is a JSON-RPC error. Everything an agent can get wrong — no such file, no session, bad regex, unknown level — comes back as a normal result with `isError: true` and the reason as text, because that is what the agent actually reads.

Logs live in named sessions, each a whole `App` with its own filters; `session` defaults to `"default"`, which is also where files named on the command line land. `open_logs` replaces that session unless `add: true`. `close_logs` frees it — a million-line log is a GB of memory.

Output is budgeted. `limit` defaults to 50 and caps at 500, a line is cut at 500 characters, and a whole reply at 64KB with a note saying so. Lists report `total`, `offset` and `next_offset`. Line tools render plain text by default (`N │ file │ text`, the file column only when more than one is loaded) because it costs a fraction of the tokens of `format: "json"`, which is there when an agent wants the timestamp and level as fields. `overview` is returned by `open_logs` and `set_filters` too, so a change needs no second call.

`set_filters` leaves out what you leave out, clears a field with `null`, and clears everything when called with no fields. Everything is validated before anything is applied, so a bad regex changes nothing. `patterns` and `lines` also take `matching`, a regex for that one call that does not touch session state.

Stdin is the transport, so it is never read as a log in this mode; diagnostics go to stderr. `config::init()` still runs, so masks and macros are the same ones the TUI uses.

```json
{ "mcpServers": { "sawmill": { "command": "/home/ggstuart/Projects/Sawmill/target/release/sawmill", "args": ["--mcp"] } } }
```

## Why Rust

`cargo` 1.94 is installed; `go` is not. Gordon already has a Rust workspace at `~/AI/yule`. ratatui 0.30 / crossterm 0.29 / regex / chrono (no default features, `clock` + `std`) / clap derive / toml 0.9 + serde / serde_json / glob. Edition 2024.

Seed material is the bash tools in `~/Code/ggtools/ggtools/`: `logplot` (314), `logstat` (55), `logtint` (28), `loguniq` (5). Do not modify ggtools. Sawmill replaces them; it does not wrap them.

## Layout

| File | Role |
|---|---|
| `src/parse.rs` | Timestamp, severity, normalisation, typed time ranges (`parse_range`). Pure functions. |
| `src/app.rs` | State: filters, `visible` entry indices, grouping, cached histogram, cursors, find hits, prompt. No terminal types. |
| `src/config.rs` | Config file: masks, macros, highlight rules, `[ui]`. `DEFAULT` is the built-in TOML. Placeholder characters. |
| `src/theme.rs` | Nineteen themes. `terminal` is the ANSI palette, Monokai is the original palette, and the rest are Herdr's truecolor ones. |
| `src/highlight.rs` | Highlight rules from the config as layered ratatui spans. Later rules paint over earlier ones. Colour names are resolved against the current theme. Pattern view uses the same rules; mask names replace them. |
| `src/buzz.rs` | The five buzzsaw frames drawn above the loading gauge. |
| `src/ui.rs` | ratatui draw. `histogram()` is called from here because bucket width depends on the widget width. Lists are windowed: only on-screen rows are built. |
| `src/mcp.rs` | MCP server: JSON-RPC framing, the tool catalogue and their JSON payloads. The only file that knows about the protocol. |
| `src/main.rs` | clap, config init, file/gzip/stdin, loading thread, glob expansion for Add files, key and mouse handling. |

Keep parsing and state free of ratatui so they stay unit-testable, and free of MCP for the same reason. 143 tests (140 unit, 3 in `tests/mcp_pipe.rs`), `cargo clippy --all-targets -- -D warnings` clean as of 2026-10-20. Run both before changing behaviour.

## Decisions already paid for

These came out of running it on `~/Code/ansible-testlog.log` (2MB, 37986 non-blank lines, all dated 2023-09-18 15:54:49–15:57:24). Do not regress them.

**Timestamps must start near the beginning of the line.** `parse_timestamp` only accepts a match whose start is within `STAMP_OFFSET` (4) characters, except Common Log Format which allows `CLF_OFFSET` (64) because the client address comes first and `[dd/Mon/yyyy:` is distinctive. Scanning further in is wrong: line `ActiveEnterTimestamp: Wed 2021-06-23 18:05:10 BST` is payload nested inside Ansible output. Treating it as the line's timestamp dragged the histogram back to 2021 and collapsed the whole log into one 86400s bucket. After the fix the span reads `15:54:49 → 15:57:24` in 5s buckets.

**Severity is a delimited token, not a word-boundary match.** `\berror\b` matches inside `libgpg-error` because `-` is a boundary, and `"Warning: Permanently added…"` in quoted SSH stderr is prose, not a level. The regex in `patterns().severity` requires a delimiter on both sides. Test names: `severity_ignores_hyphenated_identifiers`, `severity_ignores_quoted_prose`.

**An access log is a different kind, detected from the lines.** `parse::classify`, called from `App::set_logs`, counts quoted requests followed by a status (`"GET /path HTTP/1.1" 200`) against delimited level tokens. Web when the statuses are more numerous and there are at least three, so one quoted request inside a syslog stays a system log. `5xx` is red (`Error`), `4xx` yellow (`Warn`), `2xx` and `3xx` cyan (`Info`, gutter `2xx`), `1xx` blue (`Debug`), and a line with no status is `Other`. The histogram and `e w i d o` are the same five buckets. On a web log the status bar shows `5 4 3 2 1 0`, and those keys toggle; `3` and `2` are one filter. `overview` reports `kind`. `set_filters` accepts either set of names.

**A pattern group is labelled by its dominant severity, not its worst line.** `dominant()` in `app.rs` picks the most frequent level. `Severity::ALL` is worst-first; `max_by_key` keeps the *last* maximum, so the walk is reversed and ties go to the worse level. One stray error must not paint hundreds of benign lines red. Level tokens survive normalisation, so `[INFO] …` and `[ERROR] …` already form separate groups when the level is a real token — `dominant()` matters for lines whose severity comes from somewhere the normaliser also collapses.

**Patterns hold entry indices, not line numbers.** Blank lines are skipped, so `line_no - 1` is not an index into `entries`. The old drill-down did that and showed the wrong lines, or none, after the first blank line. Test: `drilling_in_survives_blank_lines`.

**Filters run once per change, never per frame.** `rebuild()` fills `visible` across all cores; the histogram is cached by width and a generation counter. On `~/million.log.gz` (1,000,000 lines) a regex include or a level toggle redraws in about 0.1s. Lists draw only the rows on screen, so the all-lines view of a million entries costs the same as ten.

**Histogram bars use `▄` at two units per cell**: lower unit in the foreground, upper in the background. `allocate()` gives every present severity at least one unit, so a single error in a busy bucket still shows. The old one-unit-per-cell code could round errors off the top.

**Numbers glued to a unit must collapse.** `\b\d+\b` left `30s` and `95s` as distinct patterns. The `NUM` mask uses `\b\d+(\.\d+)?` with no trailing boundary. Mask order in the config matters: timestamps, addresses and UUIDs run before the bare-number rule. The built-in `PATH` mask is commented out.

**`parse_lines` is the load-time bottleneck, not reading the file.** On `~/million.log.gz` reading and inflating the lines took under a second; running every regex over all 1,000,000 of them took ~8s single-threaded. It now splits into chunks across `available_parallelism()` and parses each in a scoped thread, same shape as `par_filter`. Total load (file + parse + group) dropped from ~8.7s to ~2.3s release-built. Test: `parses_a_chunk_boundary_without_losing_or_misnumbering_lines` checks `line_no` stays contiguous across a chunk seam.

## Known limitation — the next piece of work

On the Ansible log the top patterns are JSON fragments (`source: rpm`, `epoch: null`, `arch: x86_64`), because most lines are module payload rather than log records. That is accurate and useless for triage.

The obvious next feature is a toggle (suggest `t`) that keeps only lines `parse_timestamp` could read. On this log that cuts straight to the Ansible event lines. Put the flag on `App`, filter inside `matches()`, and add a test that a payload continuation line drops out while a leading-timestamp line stays. Do not special-case Ansible.

## Not done, and not asked for

- `README.md` is the human overview (logo, purpose, features). This file stays the agent handoff.
- MCP serves tools only: no resources, no prompts, no sampling, no progress notifications, and no batching. One request is answered before the next is read, so a slow `open_logs` blocks the client.
- No follow mode. Selections cannot span lines. Column mapping assumes one cell per character, so wide (CJK) characters throw selection off.
- Histogram columns are one cell wide. Dragging works; there is no keyboard bucket cursor, `t` covers that.
- `lines()` drops lines that fail UTF-8 (`map_while(Result::ok)`). Fine for the logs tested; a binary blob will silently truncate the file.
- Syslog dates with no year use the current year (`Utc::now().year()`). A December log opened in January will be wrong. Not hit yet.
- Whole file is loaded into memory. 2MB was instant. Do not stream until a real log forces it.

## Verify against a real log

Unit tests do not render. After a UI or parsing change:

```bash
cargo test && cargo clippy --all-targets
(sleep 2; printf 'qy') | script -qec "stty rows 30 cols 130; ./target/debug/sawmill $HOME/Code/ansible-testlog.log" /dev/null > /tmp/lv.out
```

Replay the capture and check the timeline footer still says `2023-09-18 15:54:49 → 15:57:24` and that pattern rows are not labelled `ERROR` just because a package name contains the word. `printf 'E'` before `q` exercises the error-only filter.

MCP needs no terminal, so a pipe is enough. Requests go in, one reply comes out per line:

```bash
printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"open_logs","arguments":{"paths":["~/Code/ansible-testlog.log"]}}}' \
  '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"patterns","arguments":{"limit":3}}}' \
  | ./target/release/sawmill --mcp
```

For multi-step checks, drive a detached tmux pane (`tmux new-session -d -x 130 -y 32 …`, `send-keys`, `capture-pane -p`, `-e` for colours). Mouse can be injected as SGR bytes with `send-keys -l`, for example `\e[<0;6;5M` press, `\e[<32;12;5M` drag, `\e[<0;12;5m` release. The agent shell sets `NO_COLOR=1`, which crossterm honours, so run the binary under `env -u NO_COLOR` or every capture comes out monochrome.
