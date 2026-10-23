# Sawmill

<p align="center">
  <img src="sawmill.png" alt="Sawmill - Dissecting Logs" width="480">
</p>

Sawmill is a terminal app for dissecting logs. Point it at a file, a gzip, several files, or a pipe. It shows when the log happened, which lines are really the same line, and the raw text behind each group.

Press `?` inside the app for the key list.

## Views

**Timeline.** A stacked severity histogram. Bucket width snaps from one second up to two days so the whole span fits the terminal. The bars are centred when they do not fill the width. Hover a bucket for its time span and line count. Drag to filter to that range. Double-click to keep the five buckets around the click. In Lines and All lines, a click jumps to that time. A bucket that contains a bookmark has a bright green mark above the bar.

**Patterns.** Each line is collapsed to a shape: UUIDs, addresses, paths, hex, quoted strings and numbers become placeholders. Identical shapes are ranked by how often they appear. A group is labelled by its most common severity, so one stray error does not paint a quiet pattern red.

**Lines.** `Enter` on a pattern shows the raw lines in that group. `Enter` again opens that line in the filtered stream, and once more returns to its pattern.

**All lines.** `a` lists every line that passes the current filters, in file order. In Lines and All lines, a bookmarked line has a bright green mark before its line number.

## Filters

| Key | What it does |
|---|---|
| `e` `w` `i` `d` `o` | Toggle Error, Warn, Info, Debug, Other |
| `E` `W` `I` `D` `O` | Show only that level; press again for all |
| `5` `4` `3` `2` `1` `0` | On an access log, the same toggles by HTTP status |
| `/` `\` | Include / exclude regex (empty clears it) |
| `Tab` | Cycle Include, Exclude, Find, then close |
| `c` | Case sensitivity. Regexes are case-insensitive by default |
| `t` `T` | Time range. `15:55`, `15:55..15:58`, `15:55..+5m`, `Feb 15`. Empty clears it. `T` clears it. Backspace steps back through mouse zooms |
| `f` `F` `P` `N` | Find a regex, then previous / next hit. `F` clears the find |
| `g` | Go to a line number |
| `Space` | Mark or unmark the cursor line, in Lines and All lines |
| `p` `n` | Previous / next bookmark |
| `B` | Clear bookmarks, after asking |

`%IP4%`, `%IP6%`, `%DATE%`, `%UUID%` and any other macro from the config expand inside a regex, as does `%FIND%`. `%IP4%`, `%IP6%` and `%DATE%` are the same patterns as the builtins. A line with no timestamp of its own stays with the record before it, so a traceback is filtered with its parent. The histogram still counts only lines that carry a timestamp.

## Menus

`Esc` on the pattern view opens **Files**, **Settings**, **Themes**, then **Quit**.

- **Files** adds logs by glob (`~/` is expanded), removes a loaded file, and clears filters. Stdin stays.
- **Settings** toggles line wrap, mouse, case sensitivity and power scroll, edits the include and exclude regexes, and clears filters.
- **Themes** picks a palette. The current one is marked. Nineteen are built in: Catppuccin, Catppuccin Latte, terminal, Tokyo Night, Tokyo Night Day, Dracula, Nord, Gruvbox, Gruvbox Light, One Dark, One Light, Solarized, Solarized Light, Kanagawa, Kanagawa Lotus, Rosé Pine, Rosé Pine Dawn, Vesper and Monokai. `terminal` keeps the terminal's own colours.
- **Quit** asks first. `q` does the same. Ctrl-C quits at once.

The theme and those four Settings toggles are written to `[ui]` in the config file. The rest of the file is left as it is.

## Mouse

Drag across a line to select inside it. Double-click takes a word; triple-click takes everything up to whitespace. Right-click offers Include, Exclude or Find, and the selected text replaces that filter. Scrollbars drag. `s` turns the mouse off so the terminal can copy.

Wrap (`l`) applies to the selected row. `#012` becomes a line break there, and a return arrow everywhere else.

## Several files

Overlapping time spans are merged by timestamp. Files that do not overlap are laid end to end in time order. A large gap between two of them is named in the status bar. Gzip is recognised from the file itself.

## Config

`~/.config/sawmill/config.toml`, or `$XDG_CONFIG_HOME/sawmill/config.toml` if that is set.

| Section | Role |
|---|---|
| `[[mask]]` | Regexes that collapse the variable parts of a line. They run in order. The name is shown struck through, as written |
| `[macros]` | `NAME = 'regex'` becomes `%NAME%` inside any regex |
| `[[highlight]]` | Colour rules for the text. A colour name follows the theme; `#rrggbb` and a 256-colour index stay as written |
| `[ui]` | Theme, wrap, mouse, case sensitivity, power scroll |

The file is created on first run. Leave a section out and Sawmill uses the built-in default for it. Include a section and it replaces that default. A key left out of `[ui]` keeps its default. A file that does not parse prints the error and exits 2.

## For agents

`--mcp` serves the same views over the Model Context Protocol: newline-delimited JSON-RPC on stdin and stdout. In this mode stdin is the transport, so it is not read as a log.

Tools: `open_logs`, `close_logs`, `overview`, `set_filters`, `patterns`, `pattern_lines`, `lines`, `context`, `timeline`.

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

## Running it

The best way is to use the binary.

```bash
sawmill app.log
sawmill app.log.gz
sawmill app1.log app2.log
some-command 2>&1 | sawmill
some-command 2>&1 | sawmill app.log
```

If you can't run it (ie your system isn't Linux amd64, or Windows with WSL), cargo build it:

```bash
cargo run --release -- app.log
cargo run --release -- app.log.gz
cargo run --release -- app1.log app2.log
some-command 2>&1 | cargo run --release
some-command 2>&1 | cargo run --release -- app.log
cargo run --release -- --mcp app.log
```

With neither a file nor a pipe, Sawmill exits 2. An empty file, or a pipe on its own with no lines, exits 1.
