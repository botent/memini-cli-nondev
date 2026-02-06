//! Memini CLI — an interactive TUI for chatting with OpenAI through MCP
//! servers, backed by Rice for persistent memory.
//!
//! This binary sets up a full-screen terminal UI, delegates to [`app::App`]
//! for all application logic, and tears the terminal down on exit.

mod app;
mod constants;
mod mcp;
mod openai;
mod rice;
mod util;

use std::io;
use std::time::Duration;

use anyhow::{Context, Result};
use crossterm::ExecutableCommand;
use crossterm::event;
use crossterm::event::DisableMouseCapture;
use crossterm::event::EnableMouseCapture;
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use crate::app::App;

// ── Entry point ──────────────────────────────────────────────────────

fn main() -> Result<()> {
    dotenvy::dotenv().ok();

    let mut terminal = setup_terminal()?;
    let mut app = App::new()?;

    let run_result = run_app(&mut terminal, &mut app);

    restore_terminal()?;
    run_result
}

// ── Terminal lifecycle ───────────────────────────────────────────────

/// Enable raw mode, switch to the alternate screen, and create the backend.
fn setup_terminal() -> Result<Terminal<CrosstermBackend<io::Stdout>>> {
    terminal::enable_raw_mode().context("enable raw mode")?;
    let mut stdout = io::stdout();
    stdout.execute(EnterAlternateScreen)?;
    stdout.execute(EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let terminal = Terminal::new(backend)?;
    Ok(terminal)
}

/// Restore the terminal to its original state.
fn restore_terminal() -> Result<()> {
    terminal::disable_raw_mode().context("disable raw mode")?;
    let mut stdout = io::stdout();
    stdout.execute(DisableMouseCapture)?;
    stdout.execute(LeaveAlternateScreen)?;
    Ok(())
}

/// Main draw → poll → handle loop.
fn run_app(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>, app: &mut App) -> Result<()> {
    loop {
        terminal.draw(|frame| app.draw(frame))?;

        if app.should_quit() {
            break;
        }

        if event::poll(Duration::from_millis(50))? {
            let ev = event::read()?;
            app.handle_event(ev)?;
        }
    }

    Ok(())
}
