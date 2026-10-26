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
/// line by phase, the open ships, and the sprint board — refreshing once a second.
#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Cli {
    /// The fleet project to show (default: the one enclosing the cwd — the nearest fleet.toml).
    #[arg(short = 'C', value_name = "DIR")]
    dir: Option<PathBuf>,
}

/// Top-level tabs, cycled by `Tab` / `S-Tab`. Fleet (the line, top-down by phase),
/// Ships (every open MR, ours + allies), Board (tickets + convoys).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum TabId {
    Fleet,
    Ships,
    Board,
}

impl TabId {
    pub fn all() -> [TabId; 3] {
        [TabId::Fleet, TabId::Ships, TabId::Board]
    }
    pub fn label(self) -> &'static str {
        match self {
            TabId::Fleet => "Fleet",
            TabId::Ships => "Ships",
            TabId::Board => "Board",
        }
    }
    fn step(self, delta: isize) -> Self {
        let all = Self::all();
        all[(self as isize + delta).rem_euclid(all.len() as isize) as usize]
    }
    /// The tab's panes, top to bottom — `1`..`3` focus them, `↑`/`↓` cycle them.
    pub fn panes(self) -> &'static [Pane] {
        match self {
            TabId::Fleet => &[Pane::Dispatched, Pane::Impl, Pane::Merge],
            TabId::Ships => &[Pane::Ships],
            TabId::Board => &[Pane::Tickets, Pane::Convoys],
        }
    }
}

/// Every pane on every tab. Fleet reads the line top-down: dispatched orders → builders at
/// work (impl phase) → ships being landed (merge phase).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Pane {
    Dispatched,
    Impl,
    Merge,
    Ships,
    Tickets,
    Convoys,
}

impl Pane {
    const COUNT: usize = 6;
    /// The `tab/pane` coordinate the action handler keys on.
    fn coordinate(self) -> &'static str {
        match self {
            Pane::Dispatched => "fleet/dispatched",
            Pane::Impl => "fleet/impl",
            Pane::Merge => "fleet/merge",
            Pane::Ships => "ships/list",
            Pane::Tickets => "board/tickets",
            Pane::Convoys => "board/convoys",
        }
    }
}

/// What a yank captures: the focused pane's interior (`y`) or the whole page (`Y`).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum YankScope {
    Pane,
    Page,
}

pub struct App {
    pub tab: TabId,
    /// The focused pane on each tab (indexed by `TabId`).
    focus: [Pane; 3],
    /// The row cursor of each pane (indexed by `Pane`).
    cursors: [usize; Pane::COUNT],
    /// The focused pane's rect, recorded on every draw so `y` can yank its region.
    pub focused_area: std::cell::Cell<Option<ratatui::layout::Rect>>,
    /// Transient footer notice (e.g. yank feedback); shown for ~3s.
    pub flash: Option<(String, Instant)>,
    /// Set by `y`/`Y`; the run loop captures the region on the next draw.
    pub yank_requested: Option<YankScope>,
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
            focus: TabId::all().map(|t| t.panes()[0]),
            cursors: [0; Pane::COUNT],
            focused_area: std::cell::Cell::new(None),
            flash: None,
            yank_requested: None,
            zoomed: false,
            fleet: Fleet::load(&project),
            project,
            last_refresh: Instant::now(),
            quit: false,
        }
    }

    /// The focused pane on the current tab.
    pub fn pane(&self) -> Pane {
        self.focus[self.tab as usize]
    }

    pub fn cursor(&self, pane: Pane) -> usize {
        self.cursors[pane as usize]
    }

    fn len(&self, pane: Pane) -> usize {
        let f = &self.fleet;
        match pane {
            Pane::Dispatched => f.dispatched.len(),
            Pane::Impl => f.builders.len(),
            Pane::Merge => f.landing.len(),
            Pane::Ships => f.armada.len(),
            Pane::Tickets => f.board.len(),
            Pane::Convoys => f.convoys.len(),
        }
    }

    /// The focused row's `(coordinate, fields)` for the action handler — the fields are
    /// the row's action-relevant values, passed as distinct argv entries. `None` when the
    /// focused list is empty.
    ///
    /// Field schema per coordinate (the handler reads these positionally; empty = n/a):
    ///   fleet/dispatched   ticket  crew(worker|hand)
    ///   fleet/impl         ticket  kind  tmux_session  state  mr_iid
    ///   fleet/merge        ticket  mr_iid  url  sailor_tmux(only when up)  sailor_state
    ///   ships/list         iid  ticket  pilot  url
    ///   board/tickets      key  status  convoy
    ///   board/convoys      key  title  captain_tmux(live only)
    fn focused_action(&self) -> Option<(String, Vec<String>)> {
        let pane = self.pane();
        let i = self.cursor(pane);
        let f = &self.fleet;
        let key = |k: Option<&domain::TicketKey>| k.map(|k| k.0.clone()).unwrap_or_default();
        let fields = match pane {
            Pane::Dispatched => f.dispatched.get(i).map(|o| {
                let crew = if o.order.auto_impl { "worker" } else { "hand" };
                vec![o.order.ticket.0.clone(), crew.into()]
            }),
            Pane::Impl => f.builders.get(i).map(|c| {
                let iid = f.ship_of(c.session.ticket.as_ref()).map(|s| s.ship.iid.to_string());
                vec![
                    key(c.session.ticket.as_ref()),
                    c.session.kind.as_str().into(),
                    c.session.tmux_session.clone(),
                    c.state.clone().unwrap_or_default(),
                    iid.unwrap_or_default(),
                ]
            }),
            Pane::Merge => f.landing.get(i).map(|r| {
                let tmux = r.sailor.as_ref().filter(|s| f.is_up(s)).map(|s| s.session.tmux_session.clone());
                vec![
                    key(r.ship.ticket.as_ref()),
                    r.ship.iid.to_string(),
                    r.ship.url.clone(),
                    tmux.unwrap_or_default(),
                    r.sailor.as_ref().and_then(|s| s.state.clone()).unwrap_or_default(),
                ]
            }),
            Pane::Ships => f.armada.get(i).map(|r| {
                vec![r.ship.iid.to_string(), key(r.ship.ticket.as_ref()), r.ship.pilot.clone(), r.ship.url.clone()]
            }),
            Pane::Tickets => f.board.get(i).map(|t| {
                vec![
                    t.key.0.clone(),
                    t.status.map(|s| s.as_str().to_string()).unwrap_or_default(),
                    t.convoy.as_ref().map(|c| c.0.clone()).unwrap_or_default(),
                ]
            }),
            Pane::Convoys => f.convoys.get(i).map(|c| {
                let captain = f.captain_of(&c.key).map(|s| s.session.tmux_session.clone());
                vec![c.key.0.clone(), c.title.clone().unwrap_or_default(), captain.unwrap_or_default()]
            }),
        }?;
        Some((pane.coordinate().to_string(), fields))
    }

    fn move_active_cursor(&mut self, delta: isize) {
        let pane = self.pane();
        let n = self.len(pane);
        if n == 0 {
            return;
        }
        let c = &mut self.cursors[pane as usize];
        *c = (*c as isize + delta).clamp(0, (n - 1) as isize) as usize;
    }

    /// Focus the pane at visual position `n` (top = 0) on the current tab.
    fn focus_nth_pane(&mut self, n: usize) {
        if let Some(&p) = self.tab.panes().get(n) {
            self.focus[self.tab as usize] = p;
        }
    }

    /// Move focus to the next (`+1`) / previous (`-1`) pane on the current tab, wrapping.
    fn step_pane(&mut self, delta: isize) {
        let panes = self.tab.panes();
        let at = panes.iter().position(|p| *p == self.pane()).unwrap_or(0) as isize;
        self.focus[self.tab as usize] = panes[(at + delta).rem_euclid(panes.len() as isize) as usize];
    }

    fn handle_key(&mut self, key: crossterm::event::KeyEvent) {
        if key.kind != KeyEventKind::Press {
            return;
        }
        // Ctrl-n / Ctrl-p — emacs-style aliases for ↓ / ↑ (next / previous pane). Matched before
        // the bare-keycode match below, which ignores modifiers.
        if key.modifiers.contains(crossterm::event::KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('n') => return self.step_pane(1),
                KeyCode::Char('p') => return self.step_pane(-1),
                _ => {}
            }
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
                self.tab = self.tab.step(1);
                return;
            }
            KeyCode::BackTab => {
                self.tab = self.tab.step(-1);
                return;
            }
            KeyCode::Up => {
                self.step_pane(-1);
                return;
            }
            KeyCode::Down => {
                self.step_pane(1);
                return;
            }
            KeyCode::Char(c @ '1'..='3') => {
                self.focus_nth_pane(c as usize - '1' as usize);
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
            for tab in TabId::all() {
                for &pane in tab.panes() {
                    let n = self.len(pane);
                    clamp(&mut self.cursors[pane as usize], n);
                }
            }
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
