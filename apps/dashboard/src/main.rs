mod action;
mod source;
mod ui;

use anyhow::Result;
use clap::Parser;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use std::io::stdout;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::source::Fleet;

/// A live TUI over the fleet. Reads fleetd's ledger READ-ONLY and renders the
/// cog roster, the armada, and the sprint board — refreshing once a second.
#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Cli {
    /// The fleet project to show (default: the one enclosing the cwd — the nearest fleet.toml).
    #[arg(short = 'C', value_name = "DIR")]
    dir: Option<PathBuf>,
}

/// Top-level tabs, cycled by `Tab` / `S-Tab`. Fleet (the cogs + escalations),
/// Armada (open ships), Board (tickets + convoys).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum TabId {
    Fleet,
    Armada,
    Board,
}

impl TabId {
    pub fn cycle_next(self) -> Self {
        match self {
            TabId::Fleet => TabId::Armada,
            TabId::Armada => TabId::Board,
            TabId::Board => TabId::Fleet,
        }
    }
    pub fn cycle_prev(self) -> Self {
        match self {
            TabId::Fleet => TabId::Board,
            TabId::Armada => TabId::Fleet,
            TabId::Board => TabId::Armada,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            TabId::Fleet => "Fleet",
            TabId::Armada => "Armada",
            TabId::Board => "Board",
        }
    }
    pub fn all() -> [TabId; 3] {
        [TabId::Fleet, TabId::Armada, TabId::Board]
    }
}

/// The Fleet tab's two panes: the cog roster (top) + open escalations (below).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum FleetPane {
    Cogs,
    Escalations,
}

/// The Board tab's two panes: the sprint tickets (top) + the active convoys (below).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum BoardPane {
    Tickets,
    Convoys,
}

/// What a yank captures: the focused pane's interior (`y`) or the whole page (`Y`).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum YankScope {
    Pane,
    Page,
}

pub struct App {
    pub tab: TabId,
    /// Which pane holds focus on each multi-pane tab (Armada is single-pane).
    pub fleet_pane: FleetPane,
    pub board_pane: BoardPane,
    /// The focused pane's rect, recorded on every draw so `y` can yank its region.
    pub focused_area: std::cell::Cell<Option<ratatui::layout::Rect>>,
    /// Transient footer notice (e.g. yank feedback); shown for ~3s.
    pub flash: Option<(String, Instant)>,
    /// Set by `y`/`Y`; the run loop captures the region on the next draw.
    pub yank_requested: Option<YankScope>,
    pub cogs_cursor: usize,
    pub escalations_cursor: usize,
    pub ships_cursor: usize,
    pub tickets_cursor: usize,
    pub convoys_cursor: usize,
    /// Zoom: the focused pane fills the body; `space` toggles it.
    pub zoomed: bool,
    pub project: wire::Project,
    pub fleet: Fleet,
    pub last_refresh: Instant,
    pub quit: bool,
}

impl App {
    fn new(project: wire::Project) -> Self {
        Self {
            tab: TabId::Fleet,
            fleet_pane: FleetPane::Cogs,
            board_pane: BoardPane::Tickets,
            focused_area: std::cell::Cell::new(None),
            flash: None,
            yank_requested: None,
            cogs_cursor: 0,
            escalations_cursor: 0,
            ships_cursor: 0,
            tickets_cursor: 0,
            convoys_cursor: 0,
            zoomed: false,
            fleet: Fleet::load(&project),
            project,
            last_refresh: Instant::now(),
            quit: false,
        }
    }

    /// The focused view's `(coordinate, row-fields)` for the action handler.
    /// `coordinate` is the `tab/pane` path; the fields are the focused row's
    /// action-relevant values, passed as distinct argv entries. `None` when the
    /// focused list is empty.
    ///
    /// Field schema per coordinate (the handler reads these positionally):
    ///   fleet/cogs         kind  ticket  convoy  tmux_session  state
    ///   fleet/escalations  ticket  raised_by  kind  ship_iid
    ///   armada/ships       iid  ticket  pilot  url
    ///   board/tickets      key  status  convoy
    ///   board/convoys      key  title
    fn focused_action(&self) -> Option<(String, Vec<String>)> {
        match self.tab {
            TabId::Fleet => match self.fleet_pane {
                FleetPane::Cogs => self.fleet.cogs.get(self.cogs_cursor).map(|c| {
                    let s = &c.session;
                    let args = vec![
                        s.kind.as_str().to_string(),
                        s.ticket.as_ref().map(|t| t.0.clone()).unwrap_or_default(),
                        s.convoy.as_ref().map(|c| c.0.clone()).unwrap_or_default(),
                        s.tmux_session.clone(),
                        c.state.clone().unwrap_or_default(),
                    ];
                    ("fleet/cogs".to_string(), args)
                }),
                FleetPane::Escalations => {
                    self.fleet.escalations.get(self.escalations_cursor).map(|e| {
                        let args = vec![
                            e.ticket.0.clone(),
                            e.raised_by.as_str().to_string(),
                            e.ship_iid.map(|i| i.to_string()).unwrap_or_default(),
                        ];
                        ("fleet/escalations".to_string(), args)
                    })
                }
            },
            TabId::Armada => self.fleet.armada.get(self.ships_cursor).map(|r| {
                let args = vec![
                    r.ship.iid.to_string(),
                    r.ship.ticket.as_ref().map(|t| t.0.clone()).unwrap_or_default(),
                    r.ship.pilot.clone(),
                    r.ship.url.clone(),
                ];
                ("armada/ships".to_string(), args)
            }),
            TabId::Board => match self.board_pane {
                BoardPane::Tickets => self.fleet.board.get(self.tickets_cursor).map(|t| {
                    let args = vec![
                        t.key.0.clone(),
                        t.status.map(|s| s.as_str().to_string()).unwrap_or_default(),
                        t.convoy.as_ref().map(|c| c.0.clone()).unwrap_or_default(),
                    ];
                    ("board/tickets".to_string(), args)
                }),
                BoardPane::Convoys => self.fleet.convoys.get(self.convoys_cursor).map(|c| {
                    let args = vec![c.key.0.clone(), c.title.clone().unwrap_or_default()];
                    ("board/convoys".to_string(), args)
                }),
            },
        }
    }

    /// Length of the currently-focused list (for cursor clamping).
    fn active_list_len(&self) -> usize {
        match (self.tab, self.fleet_pane, self.board_pane) {
            (TabId::Fleet, FleetPane::Cogs, _) => self.fleet.cogs.len(),
            (TabId::Fleet, FleetPane::Escalations, _) => self.fleet.escalations.len(),
            (TabId::Armada, _, _) => self.fleet.armada.len(),
            (TabId::Board, _, BoardPane::Tickets) => self.fleet.board.len(),
            (TabId::Board, _, BoardPane::Convoys) => self.fleet.convoys.len(),
        }
    }

    fn active_cursor_mut(&mut self) -> &mut usize {
        match (self.tab, self.fleet_pane, self.board_pane) {
            (TabId::Fleet, FleetPane::Cogs, _) => &mut self.cogs_cursor,
            (TabId::Fleet, FleetPane::Escalations, _) => &mut self.escalations_cursor,
            (TabId::Armada, _, _) => &mut self.ships_cursor,
            (TabId::Board, _, BoardPane::Tickets) => &mut self.tickets_cursor,
            (TabId::Board, _, BoardPane::Convoys) => &mut self.convoys_cursor,
        }
    }

    fn move_active_cursor(&mut self, delta: isize) {
        let n = self.active_list_len();
        if n == 0 {
            return;
        }
        let c = self.active_cursor_mut();
        *c = (*c as isize + delta).clamp(0, (n - 1) as isize) as usize;
    }

    /// Focus the pane at visual position `n` (top = 0) on a multi-pane tab.
    fn focus_nth_pane(&mut self, n: usize) {
        match self.tab {
            TabId::Fleet => {
                self.fleet_pane = [FleetPane::Cogs, FleetPane::Escalations]
                    .get(n)
                    .copied()
                    .unwrap_or(self.fleet_pane);
            }
            TabId::Board => {
                self.board_pane = [BoardPane::Tickets, BoardPane::Convoys]
                    .get(n)
                    .copied()
                    .unwrap_or(self.board_pane);
            }
            TabId::Armada => {}
        }
    }

    /// Toggle focus to the other pane on a two-pane tab (`↑`/`↓`).
    fn toggle_pane(&mut self) {
        match self.tab {
            TabId::Fleet => {
                self.fleet_pane = match self.fleet_pane {
                    FleetPane::Cogs => FleetPane::Escalations,
                    FleetPane::Escalations => FleetPane::Cogs,
                }
            }
            TabId::Board => {
                self.board_pane = match self.board_pane {
                    BoardPane::Tickets => BoardPane::Convoys,
                    BoardPane::Convoys => BoardPane::Tickets,
                }
            }
            TabId::Armada => {}
        }
    }

    fn handle_key(&mut self, key: crossterm::event::KeyEvent) {
        if key.kind != KeyEventKind::Press {
            return;
        }
        // Reserved navigation keys — handled here, never routed to the action handler.
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => {
                self.quit = true;
                return;
            }
            KeyCode::Char('r') => {
                self.refresh(true);
                return;
            }
            KeyCode::Char('y') => {
                self.yank_requested = Some(YankScope::Pane);
                return;
            }
            KeyCode::Char('Y') => {
                self.yank_requested = Some(YankScope::Page);
                return;
            }
            KeyCode::Char(' ') => {
                self.zoomed = !self.zoomed;
                return;
            }
            KeyCode::Tab => {
                self.tab = self.tab.cycle_next();
                return;
            }
            KeyCode::BackTab => {
                self.tab = self.tab.cycle_prev();
                return;
            }
            KeyCode::Up | KeyCode::Down => {
                self.toggle_pane();
                return;
            }
            KeyCode::Char('1') => {
                self.focus_nth_pane(0);
                return;
            }
            KeyCode::Char('2') => {
                self.focus_nth_pane(1);
                return;
            }
            KeyCode::Char('j') => {
                self.move_active_cursor(1);
                return;
            }
            KeyCode::Char('k') => {
                self.move_active_cursor(-1);
                return;
            }
            _ => {}
        }

        // Everything else → the action handler, with the focused row's fields.
        if action::is_reserved(key.code, key.modifiers) {
            return;
        }
        let Some(key_name) = action::key_name(key.code, key.modifiers) else {
            return;
        };
        let Some((coordinate, fields)) = self.focused_action() else {
            return;
        };
        action::spawn(&self.project.root, &coordinate, &key_name, fields);
    }

    fn refresh(&mut self, force: bool) {
        if force || self.last_refresh.elapsed() >= Duration::from_secs(1) {
            self.fleet = Fleet::load(&self.project);
            self.last_refresh = Instant::now();
            clamp(&mut self.cogs_cursor, self.fleet.cogs.len());
            clamp(&mut self.escalations_cursor, self.fleet.escalations.len());
            clamp(&mut self.ships_cursor, self.fleet.armada.len());
            clamp(&mut self.tickets_cursor, self.fleet.board.len());
            clamp(&mut self.convoys_cursor, self.fleet.convoys.len());
        }
    }
}

fn clamp(cursor: &mut usize, n: usize) {
    if n == 0 {
        *cursor = 0;
    } else if *cursor >= n {
        *cursor = n - 1;
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let project = wire::Project::find(cli.dir.as_deref())?; // before raw mode, so an error prints plainly

    enable_raw_mode()?;
    let mut out = stdout();
    execute!(out, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(out);
    let mut terminal = Terminal::new(backend)?;

    let result = run(&mut terminal, project);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result
}

/// Serialize a rect of a rendered frame buffer to plain text — WYSIWYG. The
/// `margin`-cell border is trimmed; trailing blank lines dropped.
fn region_text(
    buf: &ratatui::buffer::Buffer,
    area: ratatui::layout::Rect,
    margin: u16,
) -> (usize, String) {
    let inner = area.inner(ratatui::layout::Margin { horizontal: margin, vertical: margin });
    let mut lines: Vec<String> = Vec::new();
    for y in inner.top()..inner.bottom() {
        let mut line = String::new();
        for x in inner.left()..inner.right() {
            line.push_str(buf.cell((x, y)).map(|c| c.symbol()).unwrap_or(" "));
        }
        lines.push(line.trim_end().to_string());
    }
    while lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    (lines.len(), lines.join("\n") + "\n")
}

/// Pipe text to the macOS clipboard.
fn pbcopy(text: &str) -> std::io::Result<()> {
    use std::io::Write as _;
    let mut child = std::process::Command::new("pbcopy")
        .stdin(std::process::Stdio::piped())
        .spawn()?;
    child.stdin.take().expect("piped stdin").write_all(text.as_bytes())?;
    child.wait()?;
    Ok(())
}

fn run(terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>, project: wire::Project) -> Result<()> {
    let mut app = App::new(project);
    loop {
        // The yank capture must happen INSIDE the draw closure: draw() swaps the
        // terminal's buffers on return, so the frame buffer is the only place the
        // rendered cells exist.
        let yank = std::mem::take(&mut app.yank_requested);
        let mut yanked: Option<(usize, String)> = None;
        terminal.draw(|f| {
            ui::render(f, &app);
            match yank {
                Some(YankScope::Pane) => {
                    yanked = app.focused_area.get().map(|area| region_text(f.buffer_mut(), area, 1));
                }
                Some(YankScope::Page) => {
                    let area = f.area();
                    yanked = Some(region_text(f.buffer_mut(), area, 0));
                }
                None => {}
            }
        })?;
        if yank.is_some() {
            app.flash = Some(match yanked {
                Some((n, text)) => match pbcopy(&text) {
                    Ok(()) => (format!("⎘ yanked {} lines", n), Instant::now()),
                    Err(e) => (format!("yank failed: {}", e), Instant::now()),
                },
                None => ("yank: no focused pane".into(), Instant::now()),
            });
        }

        if event::poll(Duration::from_millis(250))? {
            if let Event::Key(key) = event::read()? {
                app.handle_key(key);
            }
        }

        app.refresh(false);

        if app.quit {
            return Ok(());
        }
    }
}
