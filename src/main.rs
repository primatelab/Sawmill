mod app;
mod buzz;
mod config;
mod highlight;
mod theme;
mod mcp;
mod parse;
mod ui;

use app::{App, PromptKind};
use tui_input::backend::crossterm::EventHandler;
use tui_input::InputRequest;
use clap::Parser;
use flate2::read::MultiGzDecoder;
use parse::{LogKind, Severity};
use ratatui::crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
        KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    },
    execute,
};
use std::io::{self, BufRead, IsTerminal, Read, Seek};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Sawmill — Dissecting Logs.
///
/// Interactive triage for logs: timeline, collapsed patterns, raw lines.
#[derive(Parser)]
#[command(version, about)]
struct Cli {
    /// Log files to read. A pipe is read as well, as `stdin`, and combined
    /// with these the same way. Several logs are merged by timestamp when
    /// their spans overlap, or laid end to end in time order otherwise.
    files: Vec<PathBuf>,

    /// Serve the Model Context Protocol on stdin/stdout instead of opening
    /// the TUI. Any files named here are loaded into the `default` session.
    #[arg(long)]
    mcp: bool,
}

/// An input ready to read: a name for the status bar and gauge, where it came
/// from (`None` for stdin), its reader, and its size if known (not for stdin).
struct Opened {
    name: String,
    path: Option<PathBuf>,
    reader: Box<dyn BufRead + Send>,
    size: Option<u64>,
}

fn open_path(path: &Path, n: &Arc<AtomicU64>) -> io::Result<Opened> {
    let name = path
        .file_name()
        .map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into());
    let (reader, size) = open(path, n.clone())?;
    Ok(Opened { name, path: Some(path.to_path_buf()), reader, size: Some(size) })
}

fn main() -> io::Result<()> {
    let cli = Cli::parse();
    if let Err(e) = config::init() {
        eprintln!("sawmill: {e}");
        std::process::exit(2);
    }
    if cli.mcp {
        // Stdin carries JSON-RPC here, so it is never read as a log.
        return mcp::serve(&cli.files);
    }

    let read_bytes = Arc::new(AtomicU64::new(0));
    let mut opened = Vec::new();
    for path in &cli.files {
        opened.push(open_path(path, &read_bytes)?);
    }
    // A pipe is a log of its own, whether or not files were named. Keys come
    // from the tty, so reading stdin here does not eat them.
    if !io::stdin().is_terminal() {
        let stdin = Counted { inner: io::stdin(), n: read_bytes.clone() };
        let reader = Box::new(io::BufReader::new(stdin));
        opened.push(Opened { name: "stdin".to_string(), path: None, reader, size: None });
    }
    if opened.is_empty() {
        eprintln!("sawmill: give me a file or pipe a log in. See --help.");
        std::process::exit(2);
    }

    let mut terminal = ratatui::init();
    let label = load_label(&opened);
    let Some(logs) = load(&mut terminal, opened, &read_bytes)? else {
        ratatui::restore();
        std::process::exit(130);
    };
    let mut app = App::from_logs(drop_empty_stdin(logs));
    if app.entries.is_empty() {
        ratatui::restore();
        eprintln!("sawmill: {label} has no lines to show.");
        std::process::exit(1);
    }

    execute!(io::stdout(), EnableMouseCapture)?;
    let result = run(&mut terminal, &mut app);
    let _ = execute!(io::stdout(), DisableMouseCapture);
    ratatui::restore();
    result
}

/// Read and parse every input. The slow part of loading; the TUI runs it on a
/// thread behind the gauge, the MCP server calls it straight.
fn parse_opened(opened: Vec<Opened>) -> Vec<parse::FileLog> {
    opened
        .into_iter()
        .map(|o| parse::FileLog {
            name: o.name,
            path: o.path,
            entries: parse::parse_lines(lines(o.reader).collect()),
        })
        .collect()
}

/// An empty pipe beside real files is not a log. On its own it stays, so the
/// empty-input check can say there is nothing to show.
fn drop_empty_stdin(logs: Vec<parse::FileLog>) -> Vec<parse::FileLog> {
    if logs.iter().any(|l| l.path.is_some()) {
        logs.into_iter().filter(|l| l.path.is_some() || !l.entries.is_empty()).collect()
    } else {
        logs
    }
}

fn load_label(opened: &[Opened]) -> String {
    match opened {
        [one] => one.name.clone(),
        many => format!("{} files", many.len()),
    }
}

/// Parse `opened` on a thread behind the loading gauge. `None` if Esc, `q`
/// or Ctrl-C gave up first.
fn load(
    terminal: &mut ratatui::DefaultTerminal,
    opened: Vec<Opened>,
    read_bytes: &Arc<AtomicU64>,
) -> io::Result<Option<Vec<parse::FileLog>>> {
    read_bytes.store(0, Ordering::Relaxed);
    let label = load_label(&opened);
    let total = opened.iter().map(|o| o.size).collect::<Option<Vec<_>>>().map(|v| v.iter().sum());
    let grouping = Arc::new(AtomicBool::new(false));
    let loader = std::thread::spawn({
        let grouping = grouping.clone();
        move || {
            let logs = parse_opened(opened);
            grouping.store(true, Ordering::Relaxed);
            logs
        }
    });

    while !loader.is_finished() {
        let done = read_bytes.load(Ordering::Relaxed);
        let grouping = grouping.load(Ordering::Relaxed);
        terminal.draw(|f| ui::draw_loading(f, &label, done, total, grouping))?;
        if event::poll(Duration::from_millis(50))?
            && let Event::Key(k) = event::read()?
            && (k.code == KeyCode::Esc
                || k.code == KeyCode::Char('q')
                || (k.code == KeyCode::Char('c') && k.modifiers.contains(KeyModifiers::CONTROL)))
        {
            return Ok(None);
        }
    }
    terminal.draw(|f| ui::draw_loading(f, &label, total.unwrap_or(0), total, true))?;
    match loader.join() {
        Ok(logs) => Ok(Some(logs)),
        Err(panic) => {
            ratatui::restore();
            std::panic::resume_unwind(panic);
        }
    }
}

/// Expand globs, with a leading `~` for the home directory. Directories, files
/// already loaded and repeats are skipped. Errors are collected rather than
/// stopping the rest. The prompt hands this whitespace-separated words; MCP
/// hands it one argument per path, so paths may contain spaces there.
fn expand_globs<'a>(
    words: impl Iterator<Item = &'a str>,
    loaded: &[PathBuf],
) -> (Vec<PathBuf>, Vec<String>) {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let known: Vec<PathBuf> = loaded.iter().filter_map(|p| p.canonicalize().ok()).collect();
    let mut seen = known.clone();
    let (mut paths, mut errors) = (Vec::new(), Vec::new());
    for word in words {
        let word = match (word.strip_prefix("~/"), &home) {
            (Some(rest), Some(h)) => h.join(rest).to_string_lossy().into_owned(),
            _ => word.to_string(),
        };
        let matches = match glob::glob(&word) {
            Ok(m) => m,
            Err(e) => {
                errors.push(format!("{word}: {}", e.msg));
                continue;
            }
        };
        let mut any = false;
        for p in matches.flatten() {
            if !p.is_file() {
                continue;
            }
            any = true;
            let canon = p.canonicalize().unwrap_or_else(|_| p.clone());
            if !seen.contains(&canon) {
                seen.push(canon);
                paths.push(p);
            }
        }
        if !any {
            errors.push(format!("{word}: no such file"));
        }
    }
    (paths, errors)
}

/// Load what the add-files prompt asked for, behind the gauge.
fn add_files(terminal: &mut ratatui::DefaultTerminal, app: &mut App, text: &str) -> io::Result<()> {
    let n = Arc::new(AtomicU64::new(0));
    let (paths, mut errors) = expand_globs(text.split_whitespace(), &app.loaded_paths());
    let mut opened = Vec::new();
    for p in &paths {
        match open_path(p, &n) {
            Ok(o) => opened.push(o),
            Err(e) => errors.push(format!("{}: {e}", p.display())),
        }
    }
    if !opened.is_empty() {
        match load(terminal, opened, &n)? {
            Some(logs) => app.add_logs(logs),
            None => errors.insert(0, "adding cancelled".into()),
        }
    } else if errors.is_empty() {
        errors.push("those files are already loaded".into());
    }
    if !errors.is_empty() {
        let errs = errors.join("; ");
        app.message = Some(match app.message.take() {
            Some(m) => format!("{m}; {errs}"),
            None => errs,
        });
    }
    Ok(())
}

/// Counts the bytes that pass through, for the loading gauge.
struct Counted<R> {
    inner: R,
    n: Arc<AtomicU64>,
}

impl<R: Read> Read for Counted<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let k = self.inner.read(buf)?;
        self.n.fetch_add(k as u64, Ordering::Relaxed);
        Ok(k)
    }
}

/// Gzip is detected from the magic bytes, not the file name, so a renamed
/// member still inflates. The file has to be seekable: the two-byte peek is
/// rewound before the decoder or the plain reader sees it. `n` counts file
/// bytes consumed, which for a gzip is compressed bytes, to set against the
/// returned file size.
fn open(path: &Path, n: Arc<AtomicU64>) -> io::Result<(Box<dyn BufRead + Send>, u64)> {
    let mut file = std::fs::File::open(path)?;
    let size = file.metadata()?.len();
    let mut magic = [0; 2];
    let got = file.read(&mut magic)?;
    file.seek(io::SeekFrom::Start(0))?;
    let file = Counted { inner: file, n };
    let reader: Box<dyn BufRead + Send> = if got == 2 && magic == [0x1f, 0x8b] {
        Box::new(io::BufReader::new(MultiGzDecoder::new(file)))
    } else {
        Box::new(io::BufReader::new(file))
    };
    Ok((reader, size))
}

fn lines<R: BufRead>(reader: R) -> impl Iterator<Item = String> {
    reader.lines().map_while(Result::ok)
}

/// Turns presses on the same cell in quick succession into double and triple clicks.
#[derive(Default)]
struct Clicks {
    last: Option<(Instant, u16, u16)>,
    n: u8,
}

impl Clicks {
    const WINDOW: Duration = Duration::from_millis(400);

    fn press(&mut self, col: u16, row: u16) -> u8 {
        let now = Instant::now();
        let again = self
            .last
            .is_some_and(|(t, c, r)| c == col && r == row && now.duration_since(t) < Self::WINDOW);
        self.n = if again { self.n % 3 + 1 } else { 1 };
        self.last = Some((now, col, row));
        self.n
    }
}

fn run(terminal: &mut ratatui::DefaultTerminal, app: &mut App) -> io::Result<()> {
    let mut clicks = Clicks::default();
    while !app.should_quit {
        terminal.draw(|f| ui::draw(f, app))?;
        let mouse = app.mouse;
        match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                let size = terminal.size()?;
                on_key(app, key, (size.width, size.height));
            }
            Event::Mouse(m) => {
                let size = terminal.size()?;
                on_mouse(app, m, (size.width, size.height), &mut clicks);
            }
            _ => {}
        }
        if let Some(text) = app.pending_add.take() {
            add_files(terminal, app, &text)?;
        }
        if app.mouse != mouse {
            if app.mouse {
                execute!(io::stdout(), EnableMouseCapture)?;
            } else {
                execute!(io::stdout(), DisableMouseCapture)?;
            }
        }
    }
    Ok(())
}

fn level_key(c: char, kind: LogKind) -> Option<Severity> {
    let letter = match c.to_ascii_lowercase() {
        'e' => Some(Severity::Error),
        'w' => Some(Severity::Warn),
        'i' => Some(Severity::Info),
        'd' => Some(Severity::Debug),
        'o' => Some(Severity::Other),
        _ => None,
    };
    if letter.is_some() {
        return letter;
    }
    if kind != LogKind::Web {
        return None;
    }
    match c {
        '5' => Some(Severity::Error),
        '4' => Some(Severity::Warn),
        '3' | '2' => Some(Severity::Info),
        '1' => Some(Severity::Debug),
        '0' => Some(Severity::Other),
        _ => None,
    }
}

fn on_key(app: &mut App, key: KeyEvent, screen: (u16, u16)) {
    let width = screen.0;
    app.message = None;
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    if ctrl && key.code == KeyCode::Char('c') {
        app.should_quit = true;
        return;
    }
    if app.confirm_quit {
        app.confirm_quit = false;
        app.should_quit = matches!(key.code, KeyCode::Char('y' | 'Y' | 'q' | 'Q') | KeyCode::Enter);
        return;
    }
    if app.show_help {
        app.show_help = false;
        return;
    }
    if app.menu.is_some() {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => app.menu.as_mut().unwrap().move_at(false),
            KeyCode::Down | KeyCode::Char('j') => app.menu.as_mut().unwrap().move_at(true),
            KeyCode::Enter => {
                let at = app.menu.as_ref().unwrap().at;
                app.pick_menu(at, screen);
            }
            KeyCode::Char(c) => match app.menu.as_ref().unwrap().key_item(c) {
                Some(i) => app.pick_menu(i, screen),
                None => app.menu = None,
            },
            _ => app.menu = None,
        }
        return;
    }
    if app.prompt.is_some() {
        edit_prompt(app, key);
        return;
    }

    let page = app.page_rows.max(1) as isize;
    let side = (width / 4).max(1) as isize;
    match key.code {
        KeyCode::Char('q' | 'Q') => app.confirm_quit = true,
        KeyCode::Down | KeyCode::Char('j') => app.move_selection(1),
        KeyCode::Up | KeyCode::Char('k') => app.move_selection(-1),
        KeyCode::PageDown => app.move_selection(page),
        KeyCode::PageUp => app.move_selection(-page),
        KeyCode::Home => app.jump(false),
        KeyCode::End => app.jump(true),
        KeyCode::Right => app.scroll_h(side),
        KeyCode::Left => app.scroll_h(-side),
        KeyCode::Enter => app.enter(),
        KeyCode::Esc => app.escape(screen),
        KeyCode::Backspace => app.back(),
        KeyCode::Tab => app.open_prompt(PromptKind::Exclude),
        KeyCode::Char(' ') => app.open_prompt(PromptKind::Include),
        KeyCode::Char('f' | 'F') => app.open_prompt(PromptKind::Find),
        KeyCode::Char('t' | 'T') => app.open_prompt(PromptKind::Time),
        KeyCode::Char('g' | 'G') => app.open_prompt(PromptKind::GoTo),
        KeyCode::Char('n') => app.find_next(true),
        KeyCode::Char('N') => app.find_next(false),
        KeyCode::Char('a' | 'A') => app.toggle_all_view(),
        KeyCode::Char('c' | 'C') => {
            app.toggle_case();
            app.save_prefs();
        }
        KeyCode::Char('l' | 'L') => {
            app.wrap ^= true;
            app.save_prefs();
        }
        KeyCode::Char('p' | 'P') => {
            app.power_scroll ^= true;
            app.save_prefs();
        }
        KeyCode::Char('s' | 'S') => {
            app.mouse ^= true;
            app.save_prefs();
        }
        KeyCode::Char('?') => app.show_help = true,
        KeyCode::Char(c) => match level_key(c, app.kind) {
            Some(s) if c.is_ascii_uppercase() => app.solo_level(s),
            Some(s) => app.toggle_level(s),
            None => {}
        },
        _ => {}
    }
}

/// Line editing for the include, exclude, find, time, go-to and add-files
/// prompts. `tui-input` owns the buffer. A plain terminal sends Ctrl-Backspace
/// as `^H`, which crossterm reports as Ctrl-H and `tui-input` deletes one
/// character for. Ctrl-Delete arrives as its own key, so the crate handles it.
fn edit_prompt(app: &mut App, key: KeyEvent) {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let prev_word = ctrl && matches!(key.code, KeyCode::Backspace | KeyCode::Char('h' | 'H'));
    match key.code {
        KeyCode::Enter => app.submit_prompt(),
        KeyCode::Esc => app.prompt = None,
        _ if prev_word => {
            if let Some(p) = app.prompt.as_mut() {
                p.input.handle(InputRequest::DeletePrevWord);
            }
        }
        _ => {
            if let Some(p) = app.prompt.as_mut() {
                p.input.handle_event(&Event::Key(key));
            }
        }
    }
}

fn on_mouse(app: &mut App, m: MouseEvent, screen: (u16, u16), clicks: &mut Clicks) {
    let (col, row) = (m.column, m.row);
    app.hover = Some((col, row));
    if app.menu.is_some() {
        let item = app.menu.as_ref().unwrap().item_at(col, row);
        let live = item.filter(|&i| app.menu.as_ref().unwrap().selectable(i));
        match m.kind {
            MouseEventKind::Moved => {
                if let Some(i) = live {
                    app.menu.as_mut().unwrap().at = i;
                }
            }
            // A click on an inert row, such as stdin, does nothing. A click
            // off the menu dismisses it.
            MouseEventKind::Down(MouseButton::Left) => match (item, live) {
                (_, Some(i)) => app.pick_menu(i, screen),
                (Some(_), None) => {}
                (None, _) => app.menu = None,
            },
            MouseEventKind::Down(_) => app.menu = None,
            _ => {}
        }
        return;
    }
    match m.kind {
        MouseEventKind::ScrollDown => app.move_selection(app.wheel_step()),
        MouseEventKind::ScrollUp => app.move_selection(-app.wheel_step()),
        MouseEventKind::Down(MouseButton::Left) => {
            if let Some(bar) = app.bar_at(col, row) {
                app.press_bar(bar, col, row);
            } else if let Some(g) = app.hist_geom.filter(|g| g.contains(col, row)) {
                let b = g.bucket_at(col);
                app.drag = Some((b, b));
            } else {
                let n = clicks.press(col, row);
                app.click(col, row, n);
            }
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            if app.bar_drag.is_some() {
                app.drag_bar(col, row);
            } else if let (Some(g), Some((anchor, _))) = (app.hist_geom, app.drag) {
                app.drag = Some((anchor, g.bucket_at(col)));
            } else {
                app.drag_to(col, row);
            }
        }
        MouseEventKind::Up(MouseButton::Left) => {
            app.bar_drag = None;
            if let Some((a, b)) = app.drag.take() {
                app.release_timeline(a, b);
            }
            app.release();
        }
        MouseEventKind::Down(MouseButton::Right) => {
            if app.hist_frame.is_some_and(|b| b.contains(col, row)) {
                app.open_timeline_menu(col, row, screen);
            } else {
                app.open_menu(col, row, screen);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::write::GzEncoder;
    use flate2::Compression;
    use std::io::Write;

    fn temp_path(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!("sawmill-{}-{label}.log", std::process::id()))
    }

    fn load(path: &Path) -> (Vec<String>, u64, u64) {
        let n = Arc::new(AtomicU64::new(0));
        let (reader, size) = open(path, n.clone()).unwrap();
        let got = lines(reader).collect();
        (got, n.load(Ordering::Relaxed), size)
    }

    #[test]
    fn reads_a_plain_file() {
        let path = temp_path("plain");
        std::fs::write(&path, "alpha\nbeta\n").unwrap();
        let (lines, read, size) = load(&path);
        let _ = std::fs::remove_file(&path);
        assert_eq!(lines, vec!["alpha".to_string(), "beta".to_string()]);
        assert_eq!((read, size), (11, 11));
    }

    #[test]
    fn reads_a_gzip_member_by_magic_bytes() {
        let path = temp_path("gzip");
        {
            let file = std::fs::File::create(&path).unwrap();
            let mut enc = GzEncoder::new(file, Compression::fast());
            write!(enc, "alpha\nbeta\n").unwrap();
            enc.finish().unwrap();
        }
        let (lines, read, size) = load(&path);
        let _ = std::fs::remove_file(&path);
        assert_eq!(lines, vec!["alpha".to_string(), "beta".to_string()]);
        assert_eq!(read, size, "progress counts compressed bytes");
    }

    #[test]
    fn web_digit_keys_toggle_the_status_classes() {
        assert_eq!(level_key('5', LogKind::Web), Some(Severity::Error));
        assert_eq!(level_key('4', LogKind::Web), Some(Severity::Warn));
        assert_eq!(level_key('3', LogKind::Web), Some(Severity::Info));
        assert_eq!(level_key('2', LogKind::Web), Some(Severity::Info));
        assert_eq!(level_key('1', LogKind::Web), Some(Severity::Debug));
        assert_eq!(level_key('0', LogKind::Web), Some(Severity::Other));
        assert_eq!(level_key('o', LogKind::Web), Some(Severity::Other));
        assert_eq!(level_key('e', LogKind::Web), Some(Severity::Error));
        assert_eq!(level_key('5', LogKind::System), None);
        assert_eq!(level_key('o', LogKind::System), Some(Severity::Other));
    }

    #[test]
    fn the_prompt_moves_and_deletes_by_character_and_by_word() {
        let mut app = App::new(vec![], "test".into());
        app.open_prompt(PromptKind::Find);
        let press = |app: &mut App, code, modifiers| on_key(app, KeyEvent::new(code, modifiers), (80, 24));
        for c in "one two".chars() {
            press(&mut app, KeyCode::Char(c), KeyModifiers::NONE);
        }
        press(&mut app, KeyCode::Home, KeyModifiers::NONE);
        assert_eq!(app.prompt.as_ref().unwrap().input.cursor(), 0);
        press(&mut app, KeyCode::Right, KeyModifiers::CONTROL);
        {
            let input = &app.prompt.as_ref().unwrap().input;
            assert_eq!(&input.value()[..input.cursor()], "one ");
        }
        press(&mut app, KeyCode::End, KeyModifiers::NONE);
        press(&mut app, KeyCode::Left, KeyModifiers::NONE);
        {
            let input = &app.prompt.as_ref().unwrap().input;
            assert_eq!(input.cursor(), input.value().chars().count() - 1);
        }
        press(&mut app, KeyCode::Backspace, KeyModifiers::CONTROL);
        assert_eq!(app.prompt.as_ref().unwrap().input.value(), "one o");
        press(&mut app, KeyCode::Delete, KeyModifiers::CONTROL);
        assert_eq!(app.prompt.as_ref().unwrap().input.value(), "one ");
        press(&mut app, KeyCode::Char('x'), KeyModifiers::NONE);
        press(&mut app, KeyCode::Char('h'), KeyModifiers::CONTROL);
        assert_eq!(app.prompt.as_ref().unwrap().input.value(), "one ", "Ctrl-Backspace arrives as Ctrl-H");
    }

    #[test]
    fn clicks_on_one_cell_count_up_to_three() {
        let mut c = Clicks::default();
        assert_eq!([c.press(1, 1), c.press(1, 1), c.press(1, 1), c.press(1, 1)], [1, 2, 3, 1]);
        assert_eq!(c.press(2, 1), 1, "a different cell starts again");
    }

    #[test]
    fn globs_expand_skipping_loaded_files_and_reporting_misses() {
        let dir = std::env::temp_dir().join(format!("sawmill-glob-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        for name in ["a.log", "b.log", "c.txt"] {
            std::fs::write(dir.join(name), "x\n").unwrap();
        }
        let pattern = format!("{}/*.log {}/nope.log", dir.display(), dir.display());
        let (paths, errors) = expand_globs(pattern.split_whitespace(), &[dir.join("a.log")]);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(paths, [dir.join("b.log")]);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].ends_with("nope.log: no such file"));
    }

    fn piped(bytes: &[u8]) -> Opened {
        Opened {
            name: "stdin".into(),
            path: None,
            reader: Box::new(io::Cursor::new(bytes.to_vec())),
            size: None,
        }
    }

    #[test]
    fn a_pipe_is_parsed_alongside_a_named_file() {
        let path = temp_path("beside");
        std::fs::write(&path, "2026-10-16 09:00:00 [INFO] from file\n").unwrap();
        let n = Arc::new(AtomicU64::new(0));
        let logs = drop_empty_stdin(parse_opened(vec![
            open_path(&path, &n).unwrap(),
            piped(b"2026-10-16 09:00:10 [INFO] from stdin\n"),
        ]));
        let _ = std::fs::remove_file(&path);
        let app = App::from_logs(logs);
        assert_eq!(app.files.len(), 2);
        assert_eq!(app.files[1], "stdin");
        let raw: Vec<&str> = app.entries.iter().map(|e| e.raw.as_str()).collect();
        assert_eq!(raw, ["2026-10-16 09:00:00 [INFO] from file", "2026-10-16 09:00:10 [INFO] from stdin"]);
    }

    #[test]
    fn an_empty_pipe_beside_a_file_is_dropped_and_alone_it_stays() {
        let path = temp_path("empty-pipe");
        std::fs::write(&path, "2026-10-16 09:00:00 [INFO] only file\n").unwrap();
        let n = Arc::new(AtomicU64::new(0));
        let logs = drop_empty_stdin(parse_opened(vec![open_path(&path, &n).unwrap(), piped(b"\n\n")]));
        let _ = std::fs::remove_file(&path);
        assert_eq!(logs.len(), 1);
        assert!(logs[0].path.is_some());

        let alone = drop_empty_stdin(vec![parse::FileLog { name: "stdin".into(), path: None, entries: vec![] }]);
        assert_eq!(alone.len(), 1, "a pipe on its own still reaches the empty-input check");
    }
}
