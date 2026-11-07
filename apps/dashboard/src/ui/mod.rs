//! The render layer — tab strip, per-tab panes, footer. Pure read over `App`
//! (and its `Fleet` view model); every pane is a header + a list of rows with the
//! focused row highlighted. A plain `Paragraph` per pane, scrolled just enough to keep the
//! cursor row in view (the Log pane is the long one).

use crate::source::{open_blockers, CogRow, LandingRow, OrderRow, Phase, ShipRow};
use crate::{App, Pane, TabId};
use domain::{Convoy, Event, PipelineStatus, Proposal, Proposed, ShipSnapshot, Ticket};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Tabs};
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
    if app.help {
        render_help(f);
    }
}

/// The keys, one per line: `?` shows them over the dashboard.
const HELP: &[(&str, &str)] = &[
    ("q / Esc", "quit"),
    ("r", "refresh"),
    ("Tab / S-Tab", "next / previous tab"),
    ("↑↓ ^n^p 1-6", "focus a pane"),
    ("j / k", "row down / up"),
    ("h / l", "swap a pane for its alternate (Dispatch: hand orders · Workers: hands)"),
    ("space", "zoom the focused pane"),
    ("y / Y", "yank the pane / the page"),
    ("enter", "switch to the row's tmux session"),
    ("m", "open the MR"),
    ("t", "open the ticket"),
    ("v", "read the verdict"),
    ("e", "switch to the ticket's escalation admiral"),
    ("a", "convene an admiral to investigate (Impl, Merge); accept (Proposals)"),
    ("d / w", "drop a proposal / weigh an anchored ticket"),
    ("?", "this help"),
];

fn render_help(f: &mut Frame) {
    let lines: Vec<Line> = HELP
        .iter()
        .map(|(k, what)| Line::from(vec![Span::styled(format!(" {k:<14}"), Style::default().fg(Color::Cyan)), Span::raw(*what)]))
        .collect();
    let area = f.area();
    let (w, h) = (area.width.min(84), (lines.len() as u16 + 2).min(area.height));
    let popup = Rect::new(area.x + (area.width - w) / 2, area.y + (area.height - h) / 2, w, h);
    f.render_widget(Clear, popup);
    f.render_widget(Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(" keys · ? or Esc closes ")), popup);
}

fn render_tab_strip(f: &mut Frame, area: Rect, app: &App) {
    let titles: Vec<Line> = TabId::all().iter().map(|t| Line::from(t.label())).collect();
    let selected = TabId::all().iter().position(|t| *t == app.tab).unwrap_or(0);
    // The health light: on while fleetd's last health check passed, out on a miss (the footer says
    // why), dim before the first check.
    let light = match health(&app.fleet.health) {
        Some(Ok(())) => Span::styled("●", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
        Some(Err(_)) => Span::styled("○", Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)),
        None => Span::styled("·", Style::default().fg(Color::DarkGray)),
    };
    // fleetd's mode, as its last reconcile pass ran it: red while it only plans, green while live.
    let mode = match app.fleet.tick.as_ref().and_then(|t| t.payload["dry_run"].as_bool()) {
        Some(true) => Span::styled("dry_run=true", Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)),
        Some(false) => Span::styled("dry_run=false", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
        None => Span::styled("dry_run=?", Style::default().fg(Color::DarkGray)),
    };
    let tabs = Tabs::new(titles)
        .block(Block::default().borders(Borders::ALL).title(Line::from(vec![Span::raw(" fleet: "), light, Span::raw(" | "), mode, Span::raw(" ")])))
        .select(selected)
        .highlight_style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD));
    f.render_widget(tabs, area);
}

/// The active tab's panes, stacked top to bottom (zoom shows just the focused one).
fn render_body(f: &mut Frame, area: Rect, app: &App) {
    let panes: Vec<Pane> = if app.zoomed { vec![app.pane()] } else { app.tab.panes().iter().map(|&s| app.shown(s)).collect() };
    // Every pane gets an equal share of the tab's height.
    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints(panes.iter().map(|_| Constraint::Fill(1)))
        .split(area);
    for (pane, area) in panes.into_iter().zip(areas.iter()) {
        let (title, header, rows) = pane_view(app, pane);
        let focused = pane == app.pane();
        draw_pane(f, *area, &title, focused, header, rows, app.cursor(pane));
    }
}


/// The focused pane as text — its title, header and every row, not just those in view.
pub fn pane_text(app: &App) -> (usize, String) {
    let (title, header, rows) = pane_view(app, app.pane());
    let text = |l: Line| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>();
    plain(std::iter::once(title.trim().to_string()).chain(header.into_iter().chain(rows).map(text)))
}

/// Lines as plain text: each trimmed at the end, trailing blank lines dropped; with the line count.
pub fn plain(lines: impl Iterator<Item = String>) -> (usize, String) {
    let mut lines: Vec<String> = lines.map(|l| l.trim_end().to_string()).collect();
    while lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    (lines.len(), lines.join("\n") + "\n")
}

/// A peek at the pane hidden behind this one (`h`/`l` swap them): its name and count, so you see
/// whether it holds anything before you switch. Empty for a pane with no alternate.
fn peek(app: &App, pane: Pane) -> String {
    match pane.alternate() {
        Some(alt) => format!("/ {} ({}) ", alt.peek_label(), app.len(alt)),
        None => String::new(),
    }
}

/// A pane's title, column header (if any), and rows.
fn pane_view(app: &App, pane: Pane) -> (String, Option<Line<'static>>, Vec<Line<'static>>) {
    let f = &app.fleet;
    match pane {
        Pane::Dispatched | Pane::HandOrders => (
            if pane == Pane::Dispatched {
                format!(" Dispatch · {} orders {}", app.len(pane), peek(app, pane))
            } else {
                format!(" Dispatch · {} hand orders {}", app.len(pane), peek(app, pane))
            },
            Some(header(&[
                ("Ticket", 12),
                ("Crew", 7),
                ("Phase", 15),
                ("Status", 12),
                ("Age", 6),
                ("Waiting on", 0),
            ])),
            app.orders(pane).into_iter().map(order_row).collect(),
        ),
        Pane::Impl | Pane::Hands => (
            if pane == Pane::Impl {
                format!(" Workers · {} {}", f.workers.len(), peek(app, pane))
            } else {
                format!(" Hands · {} {}", f.hands.len(), peek(app, pane))
            },
            Some(header(&[
                ("Ticket", 12),
                ("Crew", 7),
                ("State", 13),
                ("Live", 7),
                ("MR", 7),
                ("Pipeline", 9),
                ("Age", 6),
                ("Last action", 0),
            ])),
            (if pane == Pane::Impl { &f.workers } else { &f.hands })
                .iter()
                .map(|c| builder_row(c, f.is_up(c), f.ship_of(c.session.ticket.as_ref())))
                .collect(),
        ),
        Pane::Merge => (
            format!(" Merge · {} landing ", f.landing.len()),
            Some(header(&[
                ("Ticket", 12),
                ("Sailor", 15),
                ("Live", 7),
                ("MR", 7),
                ("Pipeline", 9),
                ("Appr", 5),
                ("Armed", 6),
                ("Thr", 6),
                ("Blocked on", 14),
                ("Age", 6),
                ("Merge status", 0),
            ])),
            f.landing.iter().map(|r| landing_row(r, r.sailor.as_ref().is_some_and(|s| f.is_up(s)))).collect(),
        ),
        Pane::Reviews => (
            format!(" Reviews · {} on watch ", f.lookouts().count()),
            Some(header(&[("MR", 7), ("On", 11), ("Pilot", 14), ("Lookout", 10), ("Live", 7), ("Age", 6), ("Title", 0)])),
            f.lookouts().map(|c| review_row(c, f.is_up(c), f.ship_on(c))).collect(),
        ),
        Pane::Ships => (ships_title(app), None, ship_rows(app)),
        Pane::Tickets => (
            format!(" board · {} tickets ", f.board.len()),
            None,
            ticket_rows(&f.board, &f.held_reasons),
        ),
        Pane::Convoys => (
            format!(" Convoys · {} open · enter captain · t ticket ", f.convoys.len()),
            Some(header(&[("Convoy", 16), ("Captain", 10), ("Launched", 9), ("Open", 6), ("Ref/Conf", 9), ("Title", 0)])),
            f.convoys.iter().map(|c| convoy_row(c, f)).collect(),
        ),
        Pane::Proposals => (
            format!(
                " Proposals · {} open · {} anchored · enter read · a accept · d drop · t ticket · w weigh ",
                f.proposals.len(),
                f.held.len()
            ),
            Some(header(&[("Id", 5), ("Ticket", 12), ("From", 12), ("Convoy", 12), ("Age", 6), ("Detail", 0)])),
            f.proposals.iter().map(proposal_row).chain(f.held.iter().map(|t| held_row(t, &f.held_reasons))).collect(),
        ),
        Pane::Log => (
            format!(
                " fleetd · {} · {} · newest first ",
                if f.fleetd_up { "up" } else { "down" },
                app.project.log().strip_prefix(&app.project.root).unwrap_or(&app.project.log()).display()
            ),
            None,
            f.log.iter().map(|l| log_row(l)).collect(),
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
    let status = if app.fleet.fleetd_up { status } else { format!("fleetd down (see Log) · {status}") };
    let reply = match &app.reply {
        Some(Ok(said)) => Span::styled(format!(" › {said} "), Style::default().fg(Color::Cyan)),
        Some(Err(failed)) => Span::styled(format!(" ✗ {failed} "), Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)),
        None => Span::raw(""),
    };
    let body = format!(" {flash}{zoom}{status}  ·  ? keys");
    let sick = match health(&app.fleet.health) {
        Some(Err(why)) => Span::styled(format!(" health: {why} · lookouts held "), Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)),
        _ => Span::raw(""),
    };
    let line = Line::from(vec![sick, Span::styled(body, dim), reply]);
    f.render_widget(Paragraph::new(line), area);
}

/// fleetd's last health check: passed, or missed (why); `None` before the first one.
fn health(check: &Option<Event>) -> Option<Result<(), String>> {
    let p = &check.as_ref()?.payload;
    if p.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
        return Some(Ok(()));
    }
    let problems: Vec<&str> = p.get("problems").and_then(|v| v.as_array()).into_iter().flatten().filter_map(|v| v.as_str()).collect();
    Some(Err(problems.join("; ")))
}

// ── the line, by phase ───────────────────────────────────────────────────────────

fn order_row(o: &OrderRow) -> Line<'static> {
    let crew = if o.order.auto_impl { "worker" } else { "hand" };
    let status = o.status.map(|s| s.as_str().to_string()).unwrap_or_else(|| "—".into());
    let mut waiting = Vec::new();
    if !o.blockers.is_empty() {
        waiting.push(format!("⛔ {}", o.blockers.join(", ")));
    }
    if let Some(reason) = &o.held {
        waiting.push(format!("⚓ {reason}")); // last: the screen clips it, a yank keeps it whole
    }
    let waiting = if waiting.is_empty() { "—".to_string() } else { waiting.join("  ") };
    let waiting_style = if waiting == "—" { dim() } else { Style::default().fg(Color::Yellow) };
    let phase_style = match o.phase {
        Phase::Waiting => yellow(),
        Phase::Implementation => Style::default(),
        Phase::Merge => Style::default().fg(Color::Cyan),
    };
    cells(vec![
        (o.order.ticket.0.clone(), 12, Style::default()),
        (crew.into(), 7, Style::default()),
        (o.phase.as_str().into(), 15, phase_style),
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
        live_cell(c.liveness.as_deref(), up),
        (mr, 7, Style::default()),
        (pipe.0, 9, pipe.1),
        (fmt_age(c.age_secs), 6, dim()),
        (format!("{anchor}{}", c.last_action.as_deref().unwrap_or("—")), 0, Style::default()),
    ])
}

fn landing_row(r: &LandingRow, up: bool) -> Line<'static> {
    let snap = r.snapshot.as_ref();
    // An ended sailor's last word stays visible, dimmed: the ship is at rest.
    let sailor = crate::source::sailor_label(r.sailor.as_ref());
    let sailor_style = if r.live { state_style(&sailor) } else { dim() };
    let age = r.sailor.as_ref().and_then(|s| s.age_secs);
    let live = if r.live { live_cell(r.sailor.as_ref().and_then(|s| s.liveness.as_deref()), up) } else { ("—".into(), 7, dim()) };
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
        Some(s) => (format!("{}/{}", s.open_threads, s.total_threads), if s.open_threads > 0 { yellow() } else { green() }),
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
        (format!("!{}", r.ship.iid), 7, Style::default()),
        (pipe.0, 9, pipe.1),
        (appr, 5, appr_style),
        (armed, 6, armed_style),
        (threads, 6, threads_style),
        (blocked, 14, blocked_style),
        (fmt_age(age), 6, dim()),
        (merge_status, 0, Style::default()),
    ])
}

/// A lookout on watch over an MR: the MR, what it's on (our ticket, or `ally`), its author, the
/// lookout's last report (`·` none yet), and the MR's title.
fn review_row(c: &CogRow, up: bool, ship: Option<&ShipRow>) -> Line<'static> {
    let state = c.state.clone().unwrap_or_else(|| "·".into());
    let title = ship.and_then(|r| r.snapshot.as_ref()).map(|s| s.title.clone()).unwrap_or_else(|| "—".into());
    let on = match ship {
        Some(r) if r.ours => r.ship.ticket.as_ref().map_or_else(|| "ours".into(), |t| t.0.clone()),
        Some(_) => "ally".into(),
        None => "—".into(),
    };
    cells(vec![
        (ship.map_or_else(|| "—".into(), |r| format!("!{}", r.ship.iid)), 7, Style::default()),
        (on, 11, Style::default()),
        (ship.map_or_else(|| "—".into(), |r| r.ship.pilot.clone()), 14, Style::default()),
        (state.clone(), 10, state_style(&state)),
        live_cell(c.liveness.as_deref(), up),
        (fmt_age(c.age_secs), 6, dim()),
        (title, 0, Style::default()),
    ])
}

// ── ships · board ────────────────────────────────────────────────────────────────

/// The Ships pane title counts the lookouts on watch, each reviewing one of these ships.
fn ships_title(app: &App) -> String {
    let lookouts = match app.fleet.lookouts().count() {
        0 => "no lookout on watch".into(),
        n => format!("{n} lookout{} on watch", if n == 1 { "" } else { "s" }),
    };
    format!(" ships · {} open · {} ", app.fleet.armada.len(), lookouts)
}

/// One line per ship; the last column is its review: `◎` a lookout is on it, `✓` reviewed at its
/// head, `·` not yet.
fn ship_rows(app: &App) -> Vec<Line<'static>> {
    app.fleet
        .armada
        .iter()
        .map(|r| {
            let watched = app.fleet.lookout_on(&r.ship.id).is_some();
            let owner = r.ship.ticket.as_ref().map(|t| t.0.clone()).unwrap_or_else(|| format!("@{}", r.ship.pilot));
            let (state, pipe, appr, reviewed) = match &r.snapshot {
                Some(s) => (
                    format!("{:?}", s.state).to_lowercase(),
                    s.pipeline.map(|p| format!("{:?}", p).to_lowercase()).unwrap_or_else(|| "—".into()),
                    format!("{}/{}", s.approvals.current, s.approvals.required),
                    if watched {
                        "◎"
                    } else if r.ship.reviewed.as_deref() == Some(s.head_sha.as_str()) {
                        "✓"
                    } else {
                        "·"
                    },
                ),
                None => ("—".into(), "—".into(), "—".into(), "·"),
            };
            let sailor = &r.sailor;
            Line::from(format!(
                "!{:<6} {:<14} {:<7} {:<8} {:>5} {:<9} {}",
                r.ship.iid, owner, state, pipe, appr, sailor, reviewed,
            ))
        })
        .collect()
}

/// One line per ticket; an anchored one shows `⚓ <reason>`, one waiting on open blockers `⛔ <keys>`.
fn ticket_rows(board: &[Ticket], held_reasons: &HashMap<String, String>) -> Vec<Line<'static>> {
    board
        .iter()
        .map(|t| {
            let status = t.status.map(|s| s.as_str().to_string()).unwrap_or_else(|| "—".into());
            let convoy = t.convoy.as_ref().map(|c| c.0.clone()).unwrap_or_default();
            let mut gate = String::new();
            let waiting = open_blockers(t, board);
            if !waiting.is_empty() {
                gate += &format!(" ⛔ {}", waiting.join(", "));
            }
            if t.held {
                let reason = held_reasons.get(&t.key.0).map(String::as_str).unwrap_or("");
                gate += &format!(" ⚓ {reason}"); // last: the screen clips it, a yank keeps it whole
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

/// An open convoy: its captain (the cog that dispatches it) while one is live — `enter` switches
/// to it — and how many of its tickets are still open (not done / cancelled).
fn convoy_row(c: &Convoy, f: &crate::source::Fleet) -> Line<'static> {
    let members: Vec<&Ticket> = f.board.iter().filter(|t| t.convoy.as_ref() == Some(&c.key)).collect();
    let open = members
        .iter()
        .filter(|t| !matches!(t.status, Some(domain::TicketStatus::Done) | Some(domain::TicketStatus::Cancelled)))
        .count();
    let live = match f.captain_of(&c.key) {
        Some(cap) if cap.state.as_deref() == Some("escalated") => ("escalated".into(), 10, state_style("escalated")),
        Some(cap) => live_cell(cap.liveness.as_deref(), f.is_up(cap)),
        None => ("off".into(), 7, dim()),
    };
    let anchor = if c.anchored_at.is_some() { "⚓ " } else { "" };
    let launched = fmt_age(f.captain_launched.get(&c.key.0).and_then(|at| age_of(at)));
    cells(vec![
        (if c.no > 0 { format!("#{} {}", c.no, c.key.0) } else { c.key.0.clone() }, 16, Style::default()),
        (live.0, 10, live.2),
        (launched, 9, dim()),
        (format!("{open}/{}", members.len()), 6, Style::default()),
        match f.verdicts.get(&c.key.0) {
            Some((refuted, confirmed)) => (format!("{refuted}/{confirmed}"), 9, Style::default()),
            None => ("—".into(), 9, dim()),
        },
        (format!("{anchor}{}", c.title.as_deref().unwrap_or("")), 0, Style::default()),
    ])
}

/// Proposals · a proposal: accept or drop it. A draft held before proposals were filed on propose
/// has no ticket yet.
fn proposal_row(p: &Proposal) -> Line<'static> {
    let key = |k: Option<&String>| k.cloned().unwrap_or_else(|| "—".into());
    let (ticket, detail) = match &p.proposed {
        Proposed::New(d) => ("—".to_string(), format!("{}: {}", d.kind, d.title)),
        Proposed::Existing { ticket, why, .. } => (ticket.0.clone(), why.clone()),
    };
    cells(vec![
        (format!("#{}", p.id.0), 5, Style::default()),
        (ticket, 12, Style::default()),
        (key(p.from_ticket.as_ref().map(|k| &k.0)), 12, Style::default()),
        (key(p.proposed.convoy().map(|c| &c.0)), 12, Style::default()),
        (fmt_age(age_of(&p.created_at)), 6, dim()),
        (detail, 0, Style::default()),
    ])
}

/// Proposals · a convoy ticket a captain anchored (not Ready): fix it, then weigh it.
fn held_row(t: &Ticket, reasons: &HashMap<String, String>) -> Line<'static> {
    let reason = reasons.get(&t.key.0).map(String::as_str).filter(|r| !r.is_empty()).unwrap_or("(no reason given)");
    cells(vec![
        ("—".into(), 5, dim()),
        (t.key.0.clone(), 12, Style::default()),
        ("—".into(), 12, dim()),
        (t.convoy.as_ref().map(|c| c.0.clone()).unwrap_or_default(), 12, Style::default()),
        ("—".into(), 6, dim()),
        (format!("⚓ {reason}"), 0, yellow()),
    ])
}

/// A fleetd log line (`<date> <time> <LEVEL> <module>: <message>`): the time, the level coloured,
/// the rest as written. A line not in that shape (a panic's trailer) shows as is.
fn log_row(line: &str) -> Line<'static> {
    let plain = || Line::from(line.to_string());
    let mut words = line.split_whitespace();
    let (Some(_date), Some(time), Some(level)) = (words.next(), words.next(), words.next()) else { return plain() };
    let (level_style, msg_style) = match level {
        "ERROR" => (Style::default().fg(Color::Red).add_modifier(Modifier::BOLD), Style::default().fg(Color::Red)),
        "WARN" => (yellow(), yellow()),
        "INFO" => (Style::default(), Style::default()),
        _ => return plain(),
    };
    let msg = line.split_once(level).map(|(_, m)| m.trim_start()).unwrap_or("");
    Line::from(vec![
        Span::styled(format!("{time} "), dim()),
        Span::styled(format!("{level:<5} "), level_style),
        Span::styled(msg.to_string(), msg_style),
    ])
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
    // Scroll just enough that the cursor row stays inside the border (and under the header).
    let visible = (area.height.saturating_sub(2) as usize).saturating_sub(header.is_some() as usize).max(1);
    let offset = (cursor + 1).saturating_sub(visible);
    let rows = rows
        .into_iter()
        .enumerate()
        .skip(offset)
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

/// The liveness cell: fleetd's read of the cog's tmux (active / quiet / idle / dead) — or, with
/// fleetd unreachable, the dashboard's own probe (`up` / `dead`). `enter` lands only on a live one.
fn live_cell(liveness: Option<&str>, up: bool) -> (String, usize, Style) {
    let live = match liveness {
        Some(l) => l.to_lowercase(),
        None if up => "up".into(),
        None => "dead".into(),
    };
    let style = match live.as_str() {
        "active" | "up" => green(),
        "quiet" => yellow(),
        "dead" => Style::default().fg(Color::Red),
        _ => dim(),
    };
    (live, 7, style)
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
        "flagged" => yellow().add_modifier(Modifier::BOLD),
        "triage" | "ruled" => Style::default().fg(Color::Cyan),
        "review" => Style::default().fg(Color::Blue),
        "anchored" => Style::default().fg(Color::Magenta),
        "concluded" | "done" | "merged" => green(),
        "sleeping" | "unmanned" | "·" => dim(),
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
