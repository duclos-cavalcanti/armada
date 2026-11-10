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
/// Ships (every open MR, ours + allies), Board (the tickets), Health (the health check, check by
/// check), Log (fleetd's log).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum TabId {
    Fleet,
    Ships,
    Board,
    Health,
    Log,
}

impl TabId {
    pub fn all() -> [TabId; 5] {
        [TabId::Fleet, TabId::Ships, TabId::Board, TabId::Health, TabId::Log]
    }
    pub fn label(self) -> &'static str {
        match self {
            TabId::Fleet => "Fleet",
            TabId::Ships => "Ships",
            TabId::Board => "Board",
            TabId::Health => "Health",
            TabId::Log => "Log",
        }
    }
    fn step(self, delta: isize) -> Self {
        let all = Self::all();
        all[(self as isize + delta).rem_euclid(all.len() as isize) as usize]
    }
    /// The tab's panes, top to bottom — `1`..`6` focus them, `↑`/`↓` cycle them. The Fleet tab
    /// opens on the open convoys, then what waits on the operator before it can enter the line.
    pub fn panes(self) -> &'static [Pane] {
        match self {
            TabId::Fleet => &[Pane::Convoys, Pane::Proposals, Pane::Dispatched, Pane::Impl, Pane::Merge, Pane::Reviews],
            TabId::Ships => &[Pane::Ships],
            TabId::Board => &[Pane::Tickets],
            TabId::Health => &[Pane::Health],
            TabId::Log => &[Pane::Log],
        }
    }
}

/// Every pane on every tab. Fleet reads the line top-down: the open convoys → what waits on the
/// operator (proposals, anchored convoy tickets) → dispatched orders → builders at work (impl phase)
/// → ships being landed (merge phase) → MRs under a lookout's review, ours and allies'.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Pane {
    Dispatched,
    HandOrders,
    Impl,
    Hands,
    Merge,
    Reviews,
    Ships,
    Tickets,
    Convoys,
    Proposals,
    Anchored,
    Health,
    Log,
}

impl Pane {
    const COUNT: usize = 13;
    /// The coordinate the action handler keys on: `tab/pane`.
    fn coordinate(self) -> &'static str {
        match self {
            Pane::Dispatched | Pane::HandOrders => "fleet/dispatched", // one row shape: an order
            Pane::Impl | Pane::Hands => "fleet/impl", // one row shape: a builder
            Pane::Merge => "fleet/merge",
            Pane::Reviews => "fleet/reviews",
            Pane::Ships => "ships/list",
            Pane::Tickets => "board/tickets",
            Pane::Convoys => "fleet/convoys",
            Pane::Proposals | Pane::Anchored => "fleet/proposals", // the row's kind tells them apart
            Pane::Health => "health/checks",
            Pane::Log => "log/fleetd",
        }
    }
    /// The pane sharing this one's slot — one shows at a time, `h`/`l` swap them.
    fn alternate(self) -> Option<Pane> {
        match self {
            Pane::Dispatched => Some(Pane::HandOrders),
            Pane::HandOrders => Some(Pane::Dispatched),
            Pane::Impl => Some(Pane::Hands),
            Pane::Hands => Some(Pane::Impl),
            Pane::Proposals => Some(Pane::Anchored),
            Pane::Anchored => Some(Pane::Proposals),
            _ => None,
        }
    }

    /// Its name in the title of the pane it alternates with, beside its count (the peek).
    pub fn peek_label(self) -> &'static str {
        match self {
            Pane::Dispatched => "Workers",
            Pane::HandOrders => "Hands",
            Pane::Impl => "Workers",
            Pane::Hands => "Hands",
            Pane::Proposals => "Proposals",
            Pane::Anchored => "Anchored",
            _ => "",
        }
    }
}

/// What a yank captures: every row of the focused pane, scrolled out of view or not (`y`), or the
/// page as drawn (`Y`).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum YankScope {
    Pane,
    Page,
}

pub struct App {
    pub tab: TabId,
    /// The focused pane on each tab (indexed by `TabId`).
    focus: [Pane; 5],
    /// The row cursor of each pane (indexed by `Pane`).
    cursors: [usize; Pane::COUNT],
    /// The slots showing their alternate pane (indexed by the slot's `Pane`).
    swapped: [bool; Pane::COUNT],
    /// What the dashboard last said back (an action's reply, a yank's) and when: `ui` boxes it low
    /// over the body for a few seconds. Set only by `notify`.
    pub notice: Option<(action::Reply, Instant)>,
    /// Where the action handler's replies arrive from its background thread.
    replies: (std::sync::mpsc::Sender<action::Reply>, std::sync::mpsc::Receiver<action::Reply>),
    /// The key help popup; `?` toggles it.
    pub help: bool,
    /// Set by `y`/`Y`; the run loop captures it on the next draw.
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
            swapped: [false; Pane::COUNT],
            notice: None,
            replies: std::sync::mpsc::channel(),
            help: false,
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
        self.shown(self.focus[self.tab as usize])
    }

    /// The pane a tab's slot shows: the slot's own, or its alternate once swapped.
    pub fn shown(&self, slot: Pane) -> Pane {
        match slot.alternate() {
            Some(alt) if self.swapped[slot as usize] => alt,
            _ => slot,
        }
    }

    pub fn cursor(&self, pane: Pane) -> usize {
        self.cursors[pane as usize]
    }

    /// The orders a dispatch pane lists: the workers' (auto-implemented), or the hands' (manual).
    pub fn orders(&self, pane: Pane) -> Vec<&source::OrderRow> {
        let hands = pane == Pane::HandOrders;
        self.fleet.dispatched.iter().filter(|o| o.order.auto_impl != hands).collect()
    }

    pub fn len(&self, pane: Pane) -> usize {
        let f = &self.fleet;
        match pane {
            Pane::Dispatched | Pane::HandOrders => self.orders(pane).len(),
            Pane::Impl => f.workers.len(),
            Pane::Hands => f.hands.len(),
            Pane::Merge => f.landing.len(),
            Pane::Reviews => f.lookouts().count(),
            Pane::Ships => f.armada.len(),
            Pane::Tickets => f.board.len(),
            Pane::Convoys => f.convoys.len(),
            Pane::Proposals => f.proposals.len(),
            Pane::Anchored => f.held.len(),
            Pane::Health => ui::health_rows(f).len(),
            Pane::Log => f.log.len(),
        }
    }

    /// The focused row's `(coordinate, fields)` for the action handler — the fields are
    /// the row's action-relevant values, passed as distinct argv entries. `None` when the
    /// focused list is empty.
    ///
    /// Field schema per coordinate (the handler reads these positionally; empty = n/a):
    ///   fleet/dispatched   ticket  crew(worker|hand)
    ///   fleet/impl         ticket  kind  tmux_session  state  mr_iid
    ///   fleet/merge        ticket  mr_iid  url  sailor_tmux  sailor_state
    ///   fleet/reviews      mr_iid  url  lookout_tmux
    ///   ships/list         iid  ticket  pilot  url  lookout_tmux
    ///   board/tickets      key  status  convoy
    ///   fleet/convoys      key  title  captain_tmux
    ///   fleet/proposals    kind(proposal|anchored)  ref(proposal id | ticket key)
    fn focused_action(&self) -> Option<(String, Vec<String>)> {
        let pane = self.pane();
        let i = self.cursor(pane);
        let f = &self.fleet;
        let key = |k: Option<&domain::TicketKey>| k.map(|k| k.0.clone()).unwrap_or_default();
        let fields = match pane {
            Pane::Dispatched | Pane::HandOrders => self.orders(pane).get(i).map(|o| {
                let crew = if o.order.auto_impl { "worker" } else { "hand" };
                vec![o.order.ticket.0.clone(), crew.into()]
            }),
            Pane::Impl | Pane::Hands => (if pane == Pane::Impl { &f.workers } else { &f.hands }).get(i).map(|c| {
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
                let tmux = r.sailor.as_ref().map(|s| s.session.tmux_session.clone());
                vec![
                    key(r.ship.ticket.as_ref()),
                    r.ship.iid.to_string(),
                    r.ship.url.clone(),
                    tmux.unwrap_or_default(),
                    r.sailor.as_ref().and_then(|s| s.state.clone()).unwrap_or_default(),
                ]
            }),
            Pane::Reviews => f.lookouts().nth(i).map(|c| {
                let ship = f.ship_on(c);
                vec![
                    ship.map(|r| r.ship.iid.to_string()).unwrap_or_default(),
                    ship.map(|r| r.ship.url.clone()).unwrap_or_default(),
                    c.session.tmux_session.clone(),
                ]
            }),
            Pane::Ships => f.armada.get(i).map(|r| {
                let lookout = f.lookout_on(&r.ship.id).map(|c| c.session.tmux_session.clone()).unwrap_or_default();
                vec![r.ship.iid.to_string(), key(r.ship.ticket.as_ref()), r.ship.pilot.clone(), r.ship.url.clone(), lookout]
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
            Pane::Proposals => f.proposals.get(i).map(|p| vec!["proposal".into(), p.id.0.to_string()]),
            Pane::Anchored => f.held.get(i).map(|t| vec!["anchored".into(), t.key.0.clone()]),
            Pane::Health | Pane::Log => None, // no row actions; `y` yanks
        }?;
        Some((pane.coordinate().to_string(), fields))
    }

    /// Say something back to the operator: every reply the dashboard shows comes through here.
    fn notify(&mut self, said: action::Reply) {
        self.notice = Some((said, Instant::now()));
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
        let at = panes.iter().position(|p| *p == self.focus[self.tab as usize]).unwrap_or(0) as isize;
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
        // The help popup takes the next key: `?` or Esc closes it, anything else is ignored.
        if self.help {
            self.help = !matches!(key.code, KeyCode::Char('?') | KeyCode::Esc);
            return;
        }
        // Reserved navigation keys — handled here, never routed to the action handler.
        match key.code {
            KeyCode::Char('?') => {
                self.help = true;
                return;
            }
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
            KeyCode::Char(c @ '1'..='6') => {
                self.focus_nth_pane(c as usize - '1' as usize);
                return;
            }
            KeyCode::Char('h' | 'l') if self.focus[self.tab as usize].alternate().is_some() => {
                let slot = self.focus[self.tab as usize];
                self.swapped[slot as usize] ^= true;
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
        action::spawn(&self.project.root, &coordinate, &key_name, fields, self.replies.0.clone());
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

/// Serialize a rect of a rendered frame buffer to plain text — WYSIWYG.
fn region_text(buf: &ratatui::buffer::Buffer, area: ratatui::layout::Rect) -> (usize, String) {
    let lines = (area.top()..area.bottom())
        .map(|y| (area.left()..area.right()).map(|x| buf.cell((x, y)).map(|c| c.symbol()).unwrap_or(" ")).collect::<String>());
    ui::plain(lines)
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
        let mut yanked: Option<(usize, String)> = (yank == Some(YankScope::Pane)).then(|| ui::pane_text(&app));
        terminal.draw(|f| {
            ui::render(f, &app);
            if yank == Some(YankScope::Page) {
                let area = f.area();
                yanked = Some(region_text(f.buffer_mut(), area));
            }
        })?;
        if let Some((n, text)) = yanked {
            app.notify(pbcopy(&text).map(|()| format!("⎘ yanked {n} lines")).map_err(|e| format!("yank failed: {e}")));
        }

        if event::poll(Duration::from_millis(250))? {
            if let Event::Key(key) = event::read()? {
                app.handle_key(key);
            }
        }

        if let Ok(r) = app.replies.1.try_recv() {
            app.notify(r);
        }
        app.refresh(false);

        if app.quit {
            return Ok(());
        }
    }
}
