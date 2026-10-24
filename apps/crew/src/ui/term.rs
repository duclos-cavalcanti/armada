use std::io::stdout;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

struct TerminalGuard;

impl TerminalGuard {
    fn new() -> Result<Self> {
        enable_raw_mode()?;
        execute!(stdout(), EnterAlternateScreen)?;
        Ok(Self {})
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(stdout(), LeaveAlternateScreen);
    }
}

pub struct TerminalState {
    guard: TerminalGuard,
    pub terminal: Terminal<CrosstermBackend<std::io::Stdout>>,
}

impl TerminalState {
    pub fn new() -> Result<Self> {
        let guard = TerminalGuard::new()?;
        let backend = CrosstermBackend::new(stdout());
        let terminal = Terminal::new(backend)?;
        Ok(Self {
            guard, 
            terminal
        })
    }

    pub fn poll(millisec_duration: u64) -> Result<Option<Event>> {
        if event::poll(Duration::from_millis(millisec_duration))? { 
            return Ok(Some(event::read()?));
        }
        Ok(None)
    }

    pub fn is_keypress(ev: Event) -> Option<KeyCode> {
        if let Event::Key(key) = ev {
            if key.kind == KeyEventKind::Press {
                return Some(key.code);
            }
        }
        None
    }

}
