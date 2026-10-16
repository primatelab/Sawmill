mod app;
mod parse;
mod ui;

use app::{App, View};
use clap::Parser;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind};
use std::io::{self, BufRead, IsTerminal};
use std::path::PathBuf;

/// Interactive triage for logs: timeline, collapsed patterns, raw lines.
#[derive(Parser)]
#[command(version, about)]
struct Cli {
    /// Log file to read. Omit to read stdin.
    file: Option<PathBuf>,
}

fn main() -> io::Result<()> {
    let cli = Cli::parse();

    let (lines, source) = match &cli.file {
        Some(path) => {
            let file = std::fs::File::open(path)?;
            let name = path
                .file_name()
                .map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into());
            (read(io::BufReader::new(file)), name)
        }
        None => {
            if io::stdin().is_terminal() {
                eprintln!("logsview: give me a file or pipe a log in. See --help.");
                std::process::exit(2);
            }
            (read(io::stdin().lock()), "stdin".to_string())
        }
    };

    if lines.is_empty() {
        eprintln!("logsview: {source} has no lines to show.");
        std::process::exit(1);
    }

    let mut app = App::new(parse::parse_lines(lines), source);
    let mut terminal = ratatui::init();
    let result = run(&mut terminal, &mut app);
    ratatui::restore();
    result
}

fn read<R: BufRead>(reader: R) -> Vec<String> {
    reader.lines().map_while(Result::ok).collect()
}

fn run(terminal: &mut ratatui::DefaultTerminal, app: &mut App) -> io::Result<()> {
    while !app.should_quit {
        terminal.draw(|f| ui::draw(f, app))?;

        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }

        match key.code {
            KeyCode::Char('q') | KeyCode::Char('Q') => app.should_quit = true,
            KeyCode::Char('f') => app.cycle_filter(),
            KeyCode::Down | KeyCode::Char('j') => app.move_selection(1),
            KeyCode::Up | KeyCode::Char('k') => app.move_selection(-1),
            KeyCode::PageDown => app.move_selection(10),
            KeyCode::PageUp => app.move_selection(-10),
            KeyCode::Enter if app.view == View::Patterns => {
                if app.selected_pattern().is_some() {
                    app.view = View::Lines;
                    app.line_scroll = 0;
                }
            }
            KeyCode::Esc | KeyCode::Backspace => app.view = View::Patterns,
            _ => {}
        }
    }
    Ok(())
}
