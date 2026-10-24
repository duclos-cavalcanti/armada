//! The render layer — tab strip, per-tab panes, footer. Pure read over `App`
//! (and its `Fleet` view model); every pane is a list of `Line`s with the focused
//! row highlighted. Lists are short (the fleet runs few cogs/ships at once), so a
//! plain `Paragraph` suffices — no scroll state for v1.

use crate::source::{CogRow, ShipRow};
use crate::{App, BoardPane, FleetPane, TabId};
use domain::{Convoy, Escalation, Event, Ticket};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Paragraph, Tabs};
use ratatui::Frame;

/// Top-level renderer: tab strip, the active tab's body, footer.
pub fn render(f: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(0), Constraint::Length(1)])
        .split(f.area());

    render_tab_strip(f, chunks[0], app);
    match app.tab {
        TabId::Fleet => render_fleet(f, chunks[1], app),
        TabId::Armada => render_armada(f, chunks[1], app),
        TabId::Board => render_board(f, chunks[1], app),
    }
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

/// Fleet tab: the cog roster over the open escalations (zoom shows just the focused one).
fn render_fleet(f: &mut Frame, area: Rect, app: &App) {
    let cogs = (FleetPane::Cogs == app.fleet_pane, cog_rows(&app.fleet.cogs));
    let esc = (FleetPane::Escalations == app.fleet_pane, escalation_rows(&app.fleet.escalations));
    if app.zoomed {
        let (focused, rows, title, cursor) = if cogs.0 {
            (true, cogs.1, cog_title(app), app.cogs_cursor)
        } else {
            (true, esc.1, format!(" escalations ({}) ", app.fleet.escalations.len()), app.escalations_cursor)
        };
        set_focus(app, area);
        draw_list(f, area, &title, focused, rows, cursor);
        return;
    }
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Ratio(2, 3), Constraint::Ratio(1, 3)])
        .split(area);
    if cogs.0 {
        set_focus(app, rows[0]);
    }
    draw_list(f, rows[0], &cog_title(app), cogs.0, cogs.1, app.cogs_cursor);
    if esc.0 {
        set_focus(app, rows[1]);
    }
    draw_list(
        f,
        rows[1],
        &format!(" escalations ({}) ", app.fleet.escalations.len()),
        esc.0,
        esc.1,
        app.escalations_cursor,
    );
}

/// Armada tab: a single pane of open ships (ours + allies).
fn render_armada(f: &mut Frame, area: Rect, app: &App) {
    set_focus(app, area);
    let title = format!(" armada · {} ships ", app.fleet.armada.len());
    draw_list(f, area, &title, true, ship_rows(&app.fleet.armada), app.ships_cursor);
}

/// Board tab: the sprint tickets over the active convoys.
fn render_board(f: &mut Frame, area: Rect, app: &App) {
    let tickets_focused = BoardPane::Tickets == app.board_pane;
    let tickets = ticket_rows(&app.fleet.board);
    let convoys = convoy_rows(&app.fleet.convoys);
    let tickets_title = format!(" board · {} tickets ", app.fleet.board.len());
    let convoys_title = format!(" convoys ({}) ", app.fleet.convoys.len());
    if app.zoomed {
        set_focus(app, area);
        if tickets_focused {
            draw_list(f, area, &tickets_title, true, tickets, app.tickets_cursor);
        } else {
            draw_list(f, area, &convoys_title, true, convoys, app.convoys_cursor);
        }
        return;
    }
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Ratio(2, 3), Constraint::Ratio(1, 3)])
        .split(area);
    if tickets_focused {
        set_focus(app, rows[0]);
    } else {
        set_focus(app, rows[1]);
    }
    draw_list(f, rows[0], &tickets_title, tickets_focused, tickets, app.tickets_cursor);
    draw_list(f, rows[1], &convoys_title, !tickets_focused, convoys, app.convoys_cursor);
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
        " {}{}{}  ·  keys: q quit · r refresh · space zoom · Tab tabs · ↑↓/1/2 pane · j/k row · y/Y yank",
        flash, zoom, status,
    );
    f.render_widget(Paragraph::new(Line::from(body)).style(dim), area);
}

// ── pane row builders ───────────────────────────────────────────────────────────

fn cog_title(app: &App) -> String {
    format!(" cogs · {} live ", app.fleet.cogs.len())
}

fn cog_rows(cogs: &[CogRow]) -> Vec<Line<'static>> {
    cogs.iter()
        .map(|c| {
            let s = &c.session;
            let subject = s
                .ticket
                .as_ref()
                .map(|t| t.0.clone())
                .or_else(|| s.convoy.as_ref().map(|c| c.0.clone()))
                .unwrap_or_else(|| "—".into());
            let state = c.state.clone().unwrap_or_else(|| "·".into());
            let live = c.liveness.as_deref().map(|l| l.to_lowercase()).unwrap_or_else(|| "—".into());
            let anchor = if s.anchored_at.is_some() { " ⚓" } else { "" };
            Line::from(format!(
                "{:<8} {:<14} {:<11} {:<7} {:>6}{}",
                s.kind.as_str(),
                subject,
                state,
                live,
                fmt_age(c.age_secs),
                anchor,
            ))
        })
        .collect()
}

fn escalation_rows(es: &[Escalation]) -> Vec<Line<'static>> {
    es.iter()
        .map(|e| {
            let ship = e.ship_iid.map(|i| format!("!{}", i)).unwrap_or_default();
            Line::from(format!(
                "{:<12} {:<8} {:<6} {}",
                e.ticket.0,
                e.raised_by.as_str(),
                ship,
                short(&e.summary, 60),
            ))
        })
        .collect()
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

fn ticket_rows(board: &[Ticket]) -> Vec<Line<'static>> {
    board
        .iter()
        .map(|t| {
            let status = t.status.map(|s| s.as_str().to_string()).unwrap_or_else(|| "—".into());
            let convoy = t.convoy.as_ref().map(|c| c.0.clone()).unwrap_or_default();
            let held = if t.held { " ✋" } else { "" };
            Line::from(format!(
                "{:<12} {:<12} {:<10} {}{}",
                t.key.0,
                status,
                convoy,
                short(t.title.as_deref().unwrap_or(""), 40),
                held,
            ))
        })
        .collect()
}

fn convoy_rows(convoys: &[Convoy]) -> Vec<Line<'static>> {
    convoys
        .iter()
        .map(|c| {
            let anchor = if c.anchored_at.is_some() { " ⚓" } else { "" };
            Line::from(format!("{:<12} {}{}", c.key.0, short(c.title.as_deref().unwrap_or(""), 50), anchor))
        })
        .collect()
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

/// Record the focused pane's rect so `y` can yank it.
fn set_focus(app: &App, area: Rect) {
    app.focused_area.set(Some(area));
}

/// Draw a bordered list pane, highlighting the cursor row when focused.
fn draw_list(f: &mut Frame, area: Rect, title: &str, focused: bool, rows: Vec<Line<'static>>, cursor: usize) {
    let block = Block::default().borders(Borders::ALL).title(title.to_string()).border_style(border_style(focused));
    let styled: Vec<Line> = rows
        .into_iter()
        .enumerate()
        .map(|(i, line)| {
            if focused && i == cursor {
                line.style(Style::default().fg(Color::Black).bg(Color::Cyan))
            } else {
                line
            }
        })
        .collect();
    f.render_widget(Paragraph::new(styled).block(block), area);
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
