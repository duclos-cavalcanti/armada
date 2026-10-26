//! The render layer — tab strip, per-tab panes, footer. Pure read over `App`
//! (and its `Fleet` view model); every pane is a header + a list of rows with the
//! focused row highlighted. Lists are short (the fleet runs few cogs/ships at once),
//! so a plain `Paragraph` suffices — no scroll state.

use crate::source::{open_blockers, CogRow, LandingRow, OrderRow, ShipRow};
use crate::{App, Pane, TabId};
use domain::{Convoy, Escalation, Event, PipelineStatus, ShipSnapshot, Ticket};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Tabs};
use ratatui::Frame;
use std::collections::HashMap;

/// Top-level renderer: tab strip, the active tab's body, footer.
pub fn render(f: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(0), Constraint::Length(1)])
        .split(f.area());

    render_tab_strip(f, chunks[0], app);
    render_body(f, chunks[1], app);
    render_footer(f, chunks[2], app);
}

fn render_tab_strip(f: &mut Frame, area: Rect, app: &App) {
    let titles: Vec<Line> = TabId::all().iter().map(|t| Line::from(t.label())).collect();
    let selected = TabId::all().iter().position(|t| *t == app.tab).unwrap_or(0);
    let tabs = Tabs::new(titles)
        .block(Block::default().borders(Borders::ALL).title(" fleet "))
        .select(selected)
        .highlight_style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD));
    f.render_widget(tabs, area);
}

/// The active tab's panes, stacked top to bottom (zoom shows just the focused one).
fn render_body(f: &mut Frame, area: Rect, app: &App) {
    let panes: Vec<Pane> = if app.zoomed { vec![app.pane()] } else { app.tab.panes().to_vec() };
    let views: Vec<_> = panes.iter().map(|p| pane_view(app, *p)).collect();
    // Each pane fits its rows (borders + header + rows); the tab's main pane takes the rest.
    let constraints = panes.iter().zip(&views).map(|(pane, (_, header, rows))| {
        if app.zoomed || is_main(*pane) {
            Constraint::Fill(1)
        } else {
            Constraint::Length((2 + header.is_some() as usize + rows.len().max(1)) as u16)
        }
    });
    let areas = Layout::default().direction(Direction::Vertical).constraints(constraints).split(area);
    for ((pane, (title, header, rows)), area) in panes.into_iter().zip(views).zip(areas.iter()) {
        let focused = pane == app.pane();
        if focused {
            app.focused_area.set(Some(*area));
        }
        draw_pane(f, *area, &title, focused, header, rows, app.cursor(pane));
    }
}

/// The pane that takes a tab's spare height — the one with the most rows to show.
fn is_main(pane: Pane) -> bool {
    matches!(pane, Pane::Merge | Pane::Ships | Pane::Tickets)
}

/// A pane's title, column header (if any), and rows.
fn pane_view(app: &App, pane: Pane) -> (String, Option<Line<'static>>, Vec<Line<'static>>) {
    let f = &app.fleet;
    match pane {
        Pane::Dispatched => (
            format!(" dispatched · {} waiting ", f.dispatched.len()),
            Some(header(&[("Ticket", 12), ("Crew", 7), ("Merge", 7), ("Status", 12), ("Age", 6), ("Waiting on", 0)])),
            f.dispatched.iter().map(order_row).collect(),
        ),
        Pane::Impl => (
            format!(" impl · {} building ", f.builders.len()),
            Some(header(&[
                ("Ticket", 12),
                ("Crew", 7),
                ("State", 13),
                ("Live", 7),
                ("Tmux", 5),
                ("MR", 7),
                ("Pipeline", 9),
                ("Age", 6),
                ("Last action", 0),
            ])),
            f.builders.iter().map(|c| builder_row(c, f.is_up(c), f.ship_of(c.session.ticket.as_ref()))).collect(),
        ),
        Pane::Merge => (
            format!(" merge · {} landing ", f.landing.len()),
            Some(header(&[
                ("Ticket", 12),
                ("Sailor", 15),
                ("Live", 7),
                ("Tmux", 5),
                ("MR", 7),
                ("Pipeline", 9),
                ("Appr", 5),
                ("Armed", 6),
                ("Thr", 4),
                ("Blocked on", 14),
                ("Age", 6),
                ("Merge status", 0),
            ])),
            f.landing.iter().map(|r| landing_row(r, r.sailor.as_ref().is_some_and(|s| f.is_up(s)))).collect(),
        ),
        Pane::Escalations => (
            format!(" escalations · {} open ", f.escalations.len()),
            None,
            escalation_rows(&f.escalations),
        ),
        Pane::Ships => (ships_title(app), None, ship_rows(&f.armada)),
        Pane::Tickets => (
            format!(" board · {} tickets ", f.board.len()),
            None,
            ticket_rows(&f.board, &f.held_reasons),
        ),
        Pane::Convoys => (
            format!(" convoys · {} active ", f.convoys.len()),
            None,
            f.convoys.iter().map(|c| convoy_row(c, f.captain_of(&c.key))).collect(),
        ),
    }
}

fn render_footer(f: &mut Frame, area: Rect, app: &App) {
    let dim = Style::default().fg(Color::DarkGray);
    let zoom = if app.zoomed { "[ZOOMED]  " } else { "" };
    let flash = app
        .flash
        .as_ref()
        .filter(|(_, at)| at.elapsed().as_secs() < 3)
        .map(|(msg, _)| format!("{}  ", msg))
        .unwrap_or_default();
    let status = match &app.fleet.load_error {
        Some(e) => format!("ledger unreachable: {}", short(e, 48)),
        None => tick_summary(&app.fleet.tick),
    };
    let body = format!(
        " {}{}{}  ·  keys: q quit · r refresh · space zoom · Tab tabs · ↑↓/^n^p/1-4 pane · j/k row · y/Y yank",
        flash, zoom, status,
    );
    f.render_widget(Paragraph::new(Line::from(body)).style(dim), area);
}

// ── the line, by phase ───────────────────────────────────────────────────────────

fn order_row(o: &OrderRow) -> Line<'static> {
    let crew = if o.order.auto_impl { "worker" } else { "hand" };
    let merge = if o.order.auto_merge { "auto" } else { "manual" };
    let status = o.status.map(|s| s.as_str().to_string()).unwrap_or_else(|| "—".into());
    let mut waiting = Vec::new();
    if let Some(reason) = &o.held {
        waiting.push(format!("✋ {}", short(reason, 40)));
    }
    if !o.blockers.is_empty() {
        waiting.push(format!("⛔ {}", o.blockers.join(", ")));
    }
    let waiting = if waiting.is_empty() { "—".to_string() } else { waiting.join("  ") };
    let waiting_style = if waiting == "—" { dim() } else { Style::default().fg(Color::Yellow) };
    cells(vec![
        (o.order.ticket.0.clone(), 12, Style::default()),
        (crew.into(), 7, Style::default()),
        (merge.into(), 7, Style::default()),
        (status, 12, Style::default()),
        (fmt_age(o.age_secs), 6, dim()),
        (waiting, 0, waiting_style),
    ])
}

fn builder_row(c: &CogRow, up: bool, ship: Option<&ShipRow>) -> Line<'static> {
    let s = &c.session;
    let state = c.state.clone().unwrap_or_else(|| "·".into());
    let anchor = if s.anchored_at.is_some() { "⚓ " } else { "" };
    let (mr, pipe) = match ship {
        Some(r) => (format!("!{}", r.ship.iid), pipeline(r.snapshot.as_ref())),
        None => ("—".into(), ("—".into(), dim())),
    };
    cells(vec![
        (s.ticket.as_ref().map(|t| t.0.clone()).unwrap_or_else(|| "—".into()), 12, Style::default()),
        (s.kind.as_str().into(), 7, Style::default()),
        (state.clone(), 13, state_style(&state)),
        live_cell(c.liveness.as_deref()),
        tmux_cell(up),
        (mr, 7, Style::default()),
        (pipe.0, 9, pipe.1),
        (fmt_age(c.age_secs), 6, dim()),
        (format!("{anchor}{}", c.last_action.as_deref().unwrap_or("—")), 0, Style::default()),
    ])
}

fn landing_row(r: &LandingRow, up: bool) -> Line<'static> {
    let snap = r.snapshot.as_ref();
    let (sailor, sailor_style, age) = match &r.sailor {
        Some(s) => {
            let state = s.state.clone().unwrap_or_else(|| "·".into());
            // An ended sailor's last word stays visible, dimmed: the ship is at rest.
            let style = if r.live { state_style(&state) } else { dim() };
            (state, style, s.age_secs)
        }
        None => ("—".into(), dim(), None),
    };
    let live = if r.live { live_cell(r.sailor.as_ref().and_then(|s| s.liveness.as_deref())) } else { ("—".into(), 7, dim()) };
    let pipe = pipeline(snap);
    let (appr, appr_style) = match snap {
        Some(s) => {
            let ok = s.approvals.current >= s.approvals.required;
            (format!("{}/{}", s.approvals.current, s.approvals.required), if ok { green() } else { yellow() })
        }
        None => ("—".into(), dim()),
    };
    let (armed, armed_style) = match snap {
        Some(s) if s.on_merge_train => ("train".into(), Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
        Some(s) if s.armed => ("armed".into(), green()),
        _ => ("—".into(), dim()),
    };
    let (threads, threads_style) = match snap {
        Some(s) => (s.open_threads.to_string(), if s.open_threads > 0 { yellow() } else { green() }),
        None => ("—".into(), dim()),
    };
    let (blocked, blocked_style) =
        if r.blocked_on.is_empty() { ("—".into(), dim()) } else { (r.blocked_on.join(","), yellow()) };
    let merge_status = match snap {
        Some(s) if s.has_conflicts => "conflict".to_string(),
        Some(s) => s.merge_status.clone().unwrap_or_else(|| "—".into()),
        None => "—".into(),
    };
    cells(vec![
        (r.ship.ticket.as_ref().map(|t| t.0.clone()).unwrap_or_default(), 12, Style::default()),
        (sailor, 15, sailor_style),
        live,
        tmux_cell(up),
        (format!("!{}", r.ship.iid), 7, Style::default()),
        (pipe.0, 9, pipe.1),
        (appr, 5, appr_style),
        (armed, 6, armed_style),
        (threads, 4, threads_style),
        (blocked, 14, blocked_style),
        (fmt_age(age), 6, dim()),
        (merge_status, 0, Style::default()),
    ])
}

fn escalation_rows(es: &[Escalation]) -> Vec<Line<'static>> {
    es.iter()
        .map(|e| {
            let ship = e.ship_iid.map(|i| format!("!{}", i)).unwrap_or_default();
            Line::from(format!("{:<12} {:<8} {:<6} {}", e.ticket.0, e.raised_by.as_str(), ship, short(&e.summary, 60)))
        })
        .collect()
}

// ── ships · board ────────────────────────────────────────────────────────────────

/// The Ships pane title carries the lookout — the singleton that reviews these ships.
fn ships_title(app: &App) -> String {
    let lookout = match app.fleet.lookout() {
        Some(l) => format!(
            "lookout {} {}",
            l.liveness.as_deref().map(str::to_lowercase).unwrap_or_else(|| "live".into()),
            fmt_age(l.age_secs)
        ),
        None => "lookout off watch".into(),
    };
    format!(" ships · {} open · {} ", app.fleet.armada.len(), lookout)
}

fn ship_rows(ships: &[ShipRow]) -> Vec<Line<'static>> {
    ships
        .iter()
        .map(|r| {
            let owner = r.ship.ticket.as_ref().map(|t| t.0.clone()).unwrap_or_else(|| format!("@{}", r.ship.pilot));
            let (state, pipe, appr, reviewed) = match &r.snapshot {
                Some(s) => (
                    format!("{:?}", s.state).to_lowercase(),
                    s.pipeline.map(|p| format!("{:?}", p).to_lowercase()).unwrap_or_else(|| "—".into()),
                    format!("{}/{}", s.approvals.current, s.approvals.required),
                    if r.ship.reviewed.as_deref() == Some(s.head_sha.as_str()) { "✓" } else { "·" },
                ),
                None => ("—".into(), "—".into(), "—".into(), "·"),
            };
            let sailor = r.sailor_state.as_deref().unwrap_or("·");
            Line::from(format!(
                "!{:<6} {:<14} {:<7} {:<8} {:>5} {:<9} {}",
                r.ship.iid, owner, state, pipe, appr, sailor, reviewed,
            ))
        })
        .collect()
}

/// One line per ticket; a held one shows `✋ <reason>`, one waiting on open blockers `⛔ <keys>`.
fn ticket_rows(board: &[Ticket], held_reasons: &HashMap<String, String>) -> Vec<Line<'static>> {
    board
        .iter()
        .map(|t| {
            let status = t.status.map(|s| s.as_str().to_string()).unwrap_or_else(|| "—".into());
            let convoy = t.convoy.as_ref().map(|c| c.0.clone()).unwrap_or_default();
            let mut gate = String::new();
            if t.held {
                let reason = held_reasons.get(&t.key.0).map(String::as_str).unwrap_or("");
                gate += &format!(" ✋ {}", short(reason, 40));
            }
            let waiting = open_blockers(t, board);
            if !waiting.is_empty() {
                gate += &format!(" ⛔ {}", waiting.join(", "));
            }
            Line::from(format!(
                "{:<12} {:<12} {:<10} {}{}",
                t.key.0,
                status,
                convoy,
                short(t.title.as_deref().unwrap_or(""), 40),
                gate.trim_end(),
            ))
        })
        .collect()
}

/// A convoy row carries its captain (the cog that dispatches it), when one is live.
fn convoy_row(c: &Convoy, captain: Option<&CogRow>) -> Line<'static> {
    let anchor = if c.anchored_at.is_some() { " ⚓" } else { "" };
    let captain = match captain {
        Some(cap) => format!(
            "  ⚑ captain {}",
            cap.liveness.as_deref().map(str::to_lowercase).unwrap_or_else(|| "live".into())
        ),
        None => String::new(),
    };
    Line::from(format!("{:<12} {}{}{}", c.key.0, short(c.title.as_deref().unwrap_or(""), 50), anchor, captain))
}

/// A one-line summary of fleetd's last reconcile tick, for the footer.
fn tick_summary(tick: &Option<Event>) -> String {
    let Some(e) = tick else { return "no tick yet".into() };
    let p = &e.payload;
    let age = fmt_age(age_of(&e.created_at));
    if p.get("dry_run").and_then(|v| v.as_bool()).unwrap_or(false) {
        let n = p.get("planned").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0);
        format!("tick: {} planned (dry-run) · {}", n, age)
    } else {
        let applied = p.get("applied").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0);
        let failed = p.get("failed").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0);
        format!("tick: {} applied, {} failed · {}", applied, failed, age)
    }
}

// ── shared helpers ───────────────────────────────────────────────────────────────

/// Draw a bordered pane: an optional column header, then the rows, the cursor row
/// highlighted when focused.
fn draw_pane(
    f: &mut Frame,
    area: Rect,
    title: &str,
    focused: bool,
    header: Option<Line<'static>>,
    rows: Vec<Line<'static>>,
    cursor: usize,
) {
    let block = Block::default().borders(Borders::ALL).title(title.to_string()).border_style(border_style(focused));
    let highlight = Style::default().fg(Color::Black).bg(Color::Cyan);
    let rows = rows
        .into_iter()
        .enumerate()
        .map(|(i, line)| if focused && i == cursor { line.patch_style(highlight) } else { line });
    let lines: Vec<Line> = header.into_iter().chain(rows).collect();
    f.render_widget(Paragraph::new(lines).block(block), area);
}

/// A row of fixed-width cells; width `0` = the last, free-running column.
fn cells(cols: Vec<(String, usize, Style)>) -> Line<'static> {
    let spans: Vec<Span> = cols
        .into_iter()
        .map(|(text, width, style)| match width {
            0 => Span::styled(text, style),
            w => Span::styled(format!("{:<w$} ", short(&text, w)), style),
        })
        .collect();
    Line::from(spans)
}

fn header(cols: &[(&str, usize)]) -> Line<'static> {
    let style = Style::default().fg(Color::DarkGray).add_modifier(Modifier::BOLD);
    cells(cols.iter().map(|(name, w)| (name.to_string(), *w, style)).collect())
}

/// The liveness cell: fleetd's tmux-probe freshness (`—` when fleetd isn't reachable).
fn live_cell(liveness: Option<&str>) -> (String, usize, Style) {
    let live = liveness.map(str::to_lowercase).unwrap_or_else(|| "—".into());
    let style = match live.as_str() {
        "active" => green(),
        "quiet" => yellow(),
        "dead" => Style::default().fg(Color::Red),
        _ => dim(),
    };
    (live, 7, style)
}

/// Is the cog's tmux session up — does `enter` land on it?
fn tmux_cell(up: bool) -> (String, usize, Style) {
    if up {
        ("up".into(), 5, green())
    } else {
        ("—".into(), 5, dim())
    }
}

fn pipeline(snap: Option<&ShipSnapshot>) -> (String, Style) {
    match snap.and_then(|s| s.pipeline) {
        Some(PipelineStatus::Success) => ("success".into(), green()),
        Some(PipelineStatus::Failed) => ("failed".into(), Style::default().fg(Color::Red)),
        Some(PipelineStatus::Running) => ("running".into(), yellow()),
        Some(p) => (format!("{:?}", p).to_lowercase(), dim()),
        None => ("—".into(), dim()),
    }
}

/// A cog's self-reported state: red when it needs you, magenta when you hold it.
fn state_style(state: &str) -> Style {
    match state {
        "escalated" => Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        "anchored" => Style::default().fg(Color::Magenta),
        "concluded" | "merged" => green(),
        "awaiting_merge" => Style::default().fg(Color::Cyan),
        "sleeping" | "·" => dim(),
        _ => Style::default(),
    }
}

fn dim() -> Style {
    Style::default().fg(Color::DarkGray)
}
fn green() -> Style {
    Style::default().fg(Color::Green)
}
fn yellow() -> Style {
    Style::default().fg(Color::Yellow)
}

fn border_style(focused: bool) -> Style {
    if focused {
        Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    }
}

/// Compact relative age from seconds: `12s` / `3m` / `2h04m` / `5d`. `—` when unknown.
pub(crate) fn fmt_age(secs: Option<i64>) -> String {
    let Some(secs) = secs else { return "—".into() };
    let secs = secs.max(0);
    if secs < 60 {
        format!("{}s", secs)
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86_400 {
        format!("{}h{:02}m", secs / 3600, (secs % 3600) / 60)
    } else {
        format!("{}d", secs / 86_400)
    }
}

/// Seconds between now and an RFC3339 timestamp (for the tick age).
fn age_of(ts: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(ts)
        .ok()
        .map(|t| (chrono::Utc::now() - t.with_timezone(&chrono::Utc)).num_seconds())
}

/// Truncate `s` to `max` chars with an ellipsis.
fn short(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(max.saturating_sub(1)).collect::<String>())
    }
}
