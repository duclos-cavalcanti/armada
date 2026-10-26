//! The scheduler — fleetd's pacer (PLAN §6), replacing the `loop/*-tick.sh` cron jobs.
//!
//! A SINGLE thread owns the ledger (the single-writer discipline) and fires tickers
//! on their cadences:
//! - **liveness** (~5s): recompute the context from a fresh host probe, publish it to
//!   the in-memory snapshot (the only place the tmux fan-out runs). Read-only → no event.
//! - **sync** (~minutes): board + armada → ledger, then record a `TickRan` event with
//!   the change-count span, so a no-op or a frozen spine is a queryable fact (#56).

use crate::config::Config;
use crate::settings;
use crate::tmux::TmuxHost;
use crate::{context, now_rfc3339, reconcile, sync, Snapshot};
use anyhow::{anyhow, Context, Result};
use domain::ports::{
    ConvoyRepo, EventLog, Forge, LookoutRepo, OrderRepo, ReportRepo, SessionHost, SessionRepo,
    ShipRepo, TicketRepo, Tracker,
};
use domain::{
    CogState, ConvoyKey, ConvoyTicketView, ConvoyView, CrewKind, EntityKind, EventKind,
    LivenessThresholds, MrDraft, Order, Packet, Report, Session, TicketKey,
};
use ledger::Ledger;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

/// A validated `fleetc launch`: open the MR for `worktree`'s branch. `default_branch` is the
/// last-resort target (the server holds it from boot).
pub struct Launch {
    pub ticket: Option<TicketKey>,
    pub worktree: String,
    pub target: Option<String>,
    pub default_branch: String,
    pub title: String,
    pub description: String,
    pub draft: bool,
}

/// The outcome channel a `Command` carries so the server can relay it as HTTP.
type Reply = std::sync::mpsc::Sender<std::result::Result<(), String>>;
/// A read command's reply — carries the composed packet (reads return data, not just a status).
type PacketReply = std::sync::mpsc::Sender<std::result::Result<Packet, String>>;
/// A convoy-view read's reply.
type ConvoyReply = std::sync::mpsc::Sender<std::result::Result<ConvoyView, String>>;
/// An MR-review read's reply — the fresh open-thread count + floating notes.
type ReviewReply = std::sync::mpsc::Sender<std::result::Result<domain::ReviewState, String>>;
/// A CI read's reply — status + failed jobs with log tails.
type CiReply = std::sync::mpsc::Sender<std::result::Result<domain::CiReport, String>>;
/// A ticket read's reply — its full text.
type TicketReply = std::sync::mpsc::Sender<std::result::Result<domain::TicketDetail, String>>;
/// A launch's reply — the new MR's URL.
type UrlReply = std::sync::mpsc::Sender<std::result::Result<String, String>>;
/// A settings-list read's reply — every live lever's effective value + source.
type SettingsReply = std::sync::mpsc::Sender<std::result::Result<Vec<crate::settings::LeverView>, String>>;

/// A write request from a client-facing thread (the server) to the single-writer
/// scheduler. Fields are already validated (parsed into `domain` types) — the lint
/// happened at the edge; this is the authoritative apply.
pub enum Command {
    RecordStatus { cog: CrewKind, ticket: TicketKey, state: CogState, last_action: Option<String>, reply: Reply },
    Dispatch { order: Order, reply: Reply },
    LaunchHand { worktree_path: String, reply: Reply }, // ticketless operator-driven hand, no order
    OpenConvoy { key: ConvoyKey, title: Option<String>, reply: Reply },
    CloseConvoy { key: ConvoyKey, reply: Reply },
    Anchor { target: ControlTarget, reply: Reply },
    Weigh { target: ControlTarget, reply: Reply },
    Sink { target: ControlTarget, reply: Reply },
    Send { target: ControlTarget, message: String, reply: Reply }, // push an operator message to the cog's pane
    SetHeld { ticket: TicketKey, held: bool, reason: Option<String>, reply: Reply },
    GetPacket { ticket: TicketKey, reply: PacketReply }, // a read: the cog's composed work-packet
    GetConvoyView { convoy: ConvoyKey, reply: ConvoyReply }, // a read: the captain's convoy slice
    GetReview { iid: i64, reply: ReviewReply }, // a read: an MR's fresh review surface (forge)
    Rebase { iid: i64, reply: Reply },          // an action: server-side rebase an MR (forge)
    Reply { cmd: wire::ReplyCmd, reply: Reply }, // an action: answer (and resolve) a review thread (forge)
    GetCi { iid: i64, reply: CiReply },         // a read: an MR's CI status + failed jobs (forge)
    RetryCi { iid: i64, reply: Reply },         // an action: re-run an MR's failed CI jobs (forge)
    Launch { launch: Launch, reply: UrlReply }, // an action: open a ticket's MR (forge)
    Arm { ticket: TicketKey, reply: Reply }, // an action: arm the ticket's MR, unless held/blocked (forge)
    GetTicket { ticket: TicketKey, reply: TicketReply }, // a read: a ticket's full text (tracker)
    NewTicket { draft: domain::TicketDraft, reply: UrlReply }, // an action: create a ticket → its key (tracker)
    BlockTicket { ticket: TicketKey, by: TicketKey, reply: Reply }, // an action: add an "is blocked by" edge (tracker)
    SetSetting { key: String, value: String, reply: Reply }, // live-config overwrite (validated here)
    GetSettings { reply: SettingsReply },                    // a read: every live lever + source
}

/// Own the ledger (single writer) + drivers and pace the tickers forever, with the
/// cadences + thresholds injected from `Config`. Between tickers it drains client
/// write `Command`s off `cmds` — the only place writes happen (single-writer).
pub fn run(
    ledger: Ledger,
    host: TmuxHost,
    forge: Box<dyn Forge + Send>,
    tracker: Box<dyn Tracker + Send>,
    snapshot: Snapshot,
    cfg: Config,
    cmds: Receiver<Command>,
) {
    let (mut next_poll, mut next_sync, mut next_reconcile) =
        (Instant::now(), Instant::now(), Instant::now());

    // The cadences + thresholds + dry_run are LIVE levers (`settings` overlay): each tick re-reads
    // the effective value off the ledger, so a `fleetc settings set` takes effect with no restart.
    // Only the startup-bound config (socket, forge target, project) is captured once above.
    loop {
        let now = Instant::now();
        if now >= next_poll {
            tick_liveness(&ledger, &host, &snapshot, &settings::thresholds(&cfg, &ledger));
            next_poll = now + Duration::from_secs(settings::poll_secs(&cfg, &ledger));
        }
        if now >= next_sync {
            if let Err(e) = tick_sync(&ledger, forge.as_ref(), tracker.as_ref()) {
                eprintln!("fleetd: sync tick error: {e}");
            }
            next_sync = now + Duration::from_secs(settings::sync_secs(&cfg, &ledger));
        }
        if now >= next_reconcile {
            // Reconcile through the executor. dry_run (live) = plan + log, execute nothing; false =
            // apply through the ports (the Gate D flip). Reuse the liveness poll's memoized cogs (no
            // extra tmux fan-out).
            let dry_run = settings::dry_run(&cfg, &ledger);
            let cogs = snapshot.lock().unwrap().clone();
            match reconcile::tick(&ledger, tracker.as_ref(), &host, forge.as_ref(), &cogs, &cfg.project.path, dry_run) {
                Ok(n) if n > 0 => {
                    let mode = if dry_run { "dry-run" } else { "live" };
                    eprintln!("fleetd: reconcile ({mode}) {n} actions");
                }
                Ok(_) => {}
                Err(e) => eprintln!("fleetd: reconcile tick error: {e}"),
            }
            next_reconcile = now + Duration::from_secs(settings::reconcile_secs(&cfg, &ledger));
        }
        // Wait for a write up to the tick quantum, then loop to re-check the tickers.
        match cmds.recv_timeout(Duration::from_millis(500)) {
            Ok(cmd) => apply(&ledger, tracker.as_ref(), &host, forge.as_ref(), &cfg, cmd),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break, // server gone → nothing left to do
        }
    }
}

/// The graceful anchor signal + the resume signal pushed into a cog's tmux pane. Sinking has no
/// message — it's a hard teardown, not a request.
const ANCHOR_MSG: &str = "⚓ anchor — finish the current step, commit WIP, report `anchored`, then stop.";
const WEIGH_MSG: &str = "⚓ weigh anchor — resume work.";

/// Apply one client command on the writer thread, replying with the outcome. `cfg` is the startup
/// base the live-config reads (`GetSettings`) resolve against.
/// Ensure a dispatched ticket exists in the ledger before its order FKs it. An off-sprint /
/// not-yet-synced ticket won't be there (the board sync pulls only open sprints), so fetch it from
/// the tracker and upsert via the same mapping the sync uses. A synced ticket is left untouched.
/// This is dispatch's only producer step — the worktree stays the operating agent's berth job.
fn ensure_ticket(ledger: &Ledger, tracker: &dyn Tracker, key: &TicketKey) -> Result<()> {
    if TicketRepo::get(ledger, key)?.is_some() {
        return Ok(());
    }
    let tt = tracker.get(key)?;
    TicketRepo::sync(ledger, &tt, &now_rfc3339())
}

fn apply(ledger: &Ledger, tracker: &dyn Tracker, host: &TmuxHost, forge: &dyn Forge, cfg: &Config, cmd: Command) {
    match cmd {
        Command::RecordStatus { cog, ticket, state, last_action, reply } => {
            let res = record_status(ledger, cog, &ticket, state, last_action)
                .map_err(|e| e.to_string());
            let _ = reply.send(res); // receiver may have hung up; nothing to do
        }
        Command::Dispatch { order, reply } => {
            // Producer self-fetch: a lone / off-sprint ticket isn't synced, so the order FK
            // (orders.ticket → tickets.key) would fail. Pull it from the tracker + upsert first.
            // The worktree is NOT dispatch's job — the operating agent berths it (fleet-berth)
            // and passes it in the order; dispatch only ensures the ledger row exists.
            let res = ensure_ticket(ledger, tracker, &order.ticket)
                .and_then(|()| ledger.dispatch(&order))
                .map_err(|e| e.to_string());
            let _ = reply.send(res);
        }
        Command::LaunchHand { worktree_path, reply } => {
            let res = reconcile::spawn_ticketless_hand(ledger, host, &worktree_path)
                .map_err(|e| e.to_string());
            let _ = reply.send(res);
        }
        Command::OpenConvoy { key, title, reply } => {
            let res = ledger.open(&key, title.as_deref(), &now_rfc3339()).map_err(|e| e.to_string());
            let _ = reply.send(res);
        }
        Command::CloseConvoy { key, reply } => {
            let res = ledger.close(&key, &now_rfc3339()).map_err(|e| e.to_string());
            let _ = reply.send(res);
        }
        Command::Anchor { target, reply } => {
            let res = control(ledger, host, Verb::Anchor, &target).map_err(|e| e.to_string());
            let _ = reply.send(res);
        }
        Command::Weigh { target, reply } => {
            let res = control(ledger, host, Verb::Weigh, &target).map_err(|e| e.to_string());
            let _ = reply.send(res);
        }
        Command::Sink { target, reply } => {
            let res = control(ledger, host, Verb::Sink, &target).map_err(|e| e.to_string());
            let _ = reply.send(res);
        }
        Command::Send { target, message, reply } => {
            let res = send(ledger, host, &target, &message).map_err(|e| e.to_string());
            let _ = reply.send(res);
        }
        Command::SetHeld { ticket, held, reason, reply } => {
            let res = set_held(ledger, forge, &ticket, held, reason.as_deref()).map_err(|e| format!("{e:#}"));
            let _ = reply.send(res);
        }
        Command::GetPacket { ticket, reply } => {
            let res = build_packet(ledger, &ticket).map_err(|e| e.to_string());
            let _ = reply.send(res);
        }
        Command::GetConvoyView { convoy, reply } => {
            let res = build_convoy_view(ledger, &convoy, forge.stacks()).map_err(|e| e.to_string());
            let _ = reply.send(res);
        }
        Command::GetReview { iid, reply } => {
            let res = forge.review_state(iid).map_err(|e| e.to_string());
            let _ = reply.send(res);
        }
        Command::Rebase { iid, reply } => {
            let res = forge.rebase(iid).map_err(|e| e.to_string());
            let _ = reply.send(res);
        }
        Command::Reply { cmd, reply } => {
            let res = forge.reply(cmd.iid, &cmd.thread, &cmd.body, cmd.resolve).map_err(|e| format!("{e:#}"));
            let _ = reply.send(res);
        }
        Command::GetCi { iid, reply } => {
            let _ = reply.send(forge.ci(iid).map_err(|e| format!("{e:#}")));
        }
        Command::RetryCi { iid, reply } => {
            let _ = reply.send(forge.retry_ci(iid).map_err(|e| format!("{e:#}")));
        }
        Command::Launch { launch: l, reply } => {
            let res = launch(ledger, forge, l).map_err(|e| format!("{e:#}"));
            let _ = reply.send(res);
        }
        Command::Arm { ticket, reply } => {
            let _ = reply.send(arm(ledger, forge, &ticket).map_err(|e| format!("{e:#}")));
        }
        Command::GetTicket { ticket, reply } => {
            let _ = reply.send(tracker.read(&ticket).map_err(|e| format!("{e:#}")));
        }
        Command::NewTicket { draft, reply } => {
            let _ = reply.send(tracker.create(&draft).map(|k| k.0).map_err(|e| format!("{e:#}")));
        }
        Command::BlockTicket { ticket, by, reply } => {
            let _ = reply.send(tracker.block(&ticket, &by).map_err(|e| format!("{e:#}")));
        }
        Command::SetSetting { key, value, reply } => {
            // Shape check (known lever + parseable type) here, authoritative, before the write.
            let res = settings::validate(&key, &value)
                .and_then(|()| ledger.setting_set(&key, &value).map_err(|e| e.to_string()));
            let _ = reply.send(res);
        }
        Command::GetSettings { reply } => {
            let _ = reply.send(Ok(settings::report(cfg, ledger)));
        }
    }
}

/// Launch a ship: open the MR for the worktree's branch. The target is the explicit one, else the
/// ticket's order fork base (a stacked child targets its parent), else origin's default branch.
fn launch(ledger: &Ledger, forge: &dyn Forge, l: Launch) -> Result<String> {
    let source = crate::git::current_branch(&l.worktree)
        .ok_or_else(|| anyhow!("{} has no branch checked out", l.worktree))?;
    let base = match (&l.target, &l.ticket) {
        (Some(t), _) => t.clone(),
        (None, Some(k)) => OrderRepo::get(ledger, k)?.map(|o| o.fork_from).unwrap_or(l.default_branch),
        (None, None) => l.default_branch,
    };
    let target = base.strip_prefix("origin/").unwrap_or(&base).to_string();
    forge.open_mr(&MrDraft { source, target, title: l.title, description: l.description, draft: l.draft })
}

/// Arm a ticket's open MR to merge once its gates pass. Refused while the ticket is held or has an
/// open blocker: the fleet's gates must hold at the moment the arm happens, not only when the sailor
/// launched (a hold or a blocker can land mid-sweep).
fn arm(ledger: &Ledger, forge: &dyn Forge, key: &TicketKey) -> Result<()> {
    let ticket = TicketRepo::get(ledger, key)?.ok_or_else(|| anyhow!("{}: not on the board", key.0))?;
    anyhow::ensure!(!ticket.held, "{} is held — not arming (fleetc unhold it first)", key.0);
    if !reconcile::blockers_terminal(ledger, &ticket.blocked_by)? {
        let keys: Vec<&str> = ticket.blocked_by.iter().map(|b| b.0.as_str()).collect();
        anyhow::bail!("{} is blocked by {} — not arming until it lands", key.0, keys.join(", "));
    }
    let ship = ledger
        .armada()?
        .into_iter()
        .find(|s| s.ticket.as_ref() == Some(key) && s.ended_at.is_none())
        .ok_or_else(|| anyhow!("{}: no open MR to arm", key.0))?;
    forge.arm_merge(ship.iid)
}

/// Hold or release a ticket. An armed MR merges on the forge's own say, past every fleet gate, so a
/// hold also acts on the ticket's open MR: disarm it, clear its `swept` (the first pass after
/// release re-sweeps: a fresh sailor re-arms, or reports `awaiting_merge`), and mark it draft — a
/// draft can't merge even if re-armed. Release marks it ready. Each forge step that fails is
/// reported with what already happened, so the caller can react (e.g. a forge without drafts).
fn set_held(ledger: &Ledger, forge: &dyn Forge, ticket: &TicketKey, held: bool, reason: Option<&str>) -> Result<()> {
    TicketRepo::set_held(ledger, ticket, held, reason, &now_rfc3339())?;
    let open = ledger.armada()?.into_iter().find(|s| s.ticket.as_ref() == Some(ticket) && s.ended_at.is_none());
    let Some(ship) = open else { return Ok(()) };
    let iid = ship.iid;
    if held {
        forge.disarm(iid).with_context(|| format!("held, but disarming !{iid} failed"))?;
        ShipRepo::set_swept(ledger, &ship.id, None)?;
        forge.set_draft(iid, true).with_context(|| format!("held + disarmed, but couldn't mark !{iid} draft"))?;
    } else {
        forge.set_draft(iid, false).with_context(|| format!("released, but couldn't mark !{iid} ready"))?;
    }
    Ok(())
}

/// Compose a captain's convoy slice: every ticket in the convoy, each marked `ready` (fleetd's
/// `is_ready` frontier) + `dispatched` (an order exists), plus a light cog/ship awareness. The
/// captain dispatches `ready && !dispatched` and closes the convoy once every ticket is terminal.
fn build_convoy_view(ledger: &Ledger, convoy: &ConvoyKey, stacks: bool) -> Result<ConvoyView> {
    let armada = ShipRepo::armada(ledger)?;
    let mut tickets = Vec::new();
    for t in TicketRepo::board(ledger)? {
        if t.convoy.as_ref() != Some(convoy) {
            continue;
        }
        let classification = crate::reconcile::classify(ledger, &t, &armada, stacks)?;
        let dispatched = OrderRepo::get(ledger, &t.key)?.is_some();
        let live_cog = SessionRepo::for_ticket(ledger, &t.key)?
            .into_iter()
            .find(|s| s.ended_at.is_none())
            .map(|s| s.kind);
        let ship_state = match armada.iter().find(|s| s.ticket.as_ref() == Some(&t.key) && s.ended_at.is_none()) {
            Some(s) => ShipRepo::snapshot(ledger, &s.id)?.map(|snap| snap.state),
            None => None,
        };
        // A STACKED ticket forks off its in-flight parent: the blocker that has an open ship.
        let fork_from = match classification {
            Some(domain::Classification::Stacked) => t.blocked_by.iter().find_map(|b| {
                armada
                    .iter()
                    .find(|s| s.ticket.as_ref() == Some(b) && s.ended_at.is_none() && !s.source.is_empty())
                    .map(|s| format!("origin/{}", s.source))
            }),
            _ => None,
        };
        tickets.push(ConvoyTicketView { key: t.key, status: t.status, classification, dispatched, live_cog, ship_state, fork_from });
    }
    Ok(ConvoyView { convoy: convoy.clone(), tickets })
}

/// Compose a cog's work-packet from the ledger: its dispatch order, its open ship + the ship's
/// latest snapshot, and the cached ticket. A pure read (the fresh-poll that keeps the snapshot
/// current happens at LaunchSailor); the sailor calls this at boot to decide its sweep.
fn build_packet(ledger: &Ledger, ticket: &TicketKey) -> Result<Packet> {
    let order = OrderRepo::get(ledger, ticket)?;
    let ship = ShipRepo::armada(ledger)?
        .into_iter()
        .find(|s| s.ticket.as_ref() == Some(ticket) && s.ended_at.is_none());
    let snapshot = match &ship {
        Some(s) => ShipRepo::snapshot(ledger, &s.id)?,
        None => None,
    };
    let ticket_entity = TicketRepo::get(ledger, ticket)?;
    Ok(Packet { order, ship, snapshot, ticket: ticket_entity })
}

/// The live cog(s) on a ticket (sessions not yet ended).
fn live_cogs_on(ledger: &Ledger, ticket: &TicketKey) -> Result<Vec<Session>> {
    Ok(SessionRepo::for_ticket(ledger, ticket)?.into_iter().filter(|s| s.ended_at.is_none()).collect())
}

/// The control verb applied by `control` to the resolved session(s).
#[derive(Clone, Copy)]
pub enum Verb {
    Anchor,
    Weigh,
    Sink,
}

/// The resolved target of a control verb (anchor/weigh/sink). The uniform `--cog <kind>` addressing
/// (§A): worker/sailor/hand are addressed by `ticket` (+ kind), a captain by its `convoy`, the
/// lookout as the singleton. Parsed + validated from the wire `CogTarget` — the daemon branches here.
pub enum ControlTarget {
    Cog { cog: CrewKind, ticket: TicketKey }, // the homogeneous ticketed surface
    Captain { convoy: ConvoyKey },
    Lookout,
}

impl ControlTarget {
    /// Validate the wire DTO: `cog` must be a known kind, and its key must match — ticketed cogs need
    /// a `ticket`, a captain a `convoy`, the lookout neither (a mismatch is a clear error at the edge).
    pub fn from_wire(t: wire::CogTarget) -> std::result::Result<Self, String> {
        let cog = CrewKind::parse(&t.cog).ok_or_else(|| format!("unknown --cog {:?}", t.cog))?;
        match cog {
            CrewKind::Worker | CrewKind::Sailor | CrewKind::Hand => {
                let ticket = t.ticket.ok_or_else(|| format!("--cog {} needs --ticket", cog.as_str()))?;
                let ticket = TicketKey::parse(&ticket).map_err(|e| e.to_string())?;
                Ok(ControlTarget::Cog { cog, ticket })
            }
            CrewKind::Captain => {
                let convoy = t.convoy.ok_or_else(|| "--cog captain needs --convoy".to_string())?;
                let convoy = ConvoyKey::parse(&convoy).map_err(|e| e.to_string())?;
                Ok(ControlTarget::Captain { convoy })
            }
            CrewKind::Lookout => Ok(ControlTarget::Lookout),
        }
    }
}

/// The live session(s) a control/send verb targets — the ONE resolution shared by anchor/weigh/sink
/// and send. Worker/sailor/hand resolve by (ticket, kind); a captain by its convoy; the lookout as
/// the singleton kind.
fn live_sessions_for(ledger: &Ledger, target: &ControlTarget) -> Result<Vec<Session>> {
    Ok(match target {
        ControlTarget::Cog { cog, ticket } => {
            live_cogs_on(ledger, ticket)?.into_iter().filter(|s| s.kind == *cog).collect()
        }
        ControlTarget::Captain { convoy } => SessionRepo::live(ledger)?
            .into_iter()
            .filter(|s| s.kind == CrewKind::Captain && s.convoy.as_ref() == Some(convoy))
            .collect(),
        ControlTarget::Lookout => {
            SessionRepo::live(ledger)?.into_iter().filter(|s| s.kind == CrewKind::Lookout).collect()
        }
    })
}

/// Push an operator message into the target cog's tmux pane(s): `send-keys` + Enter, so it submits as
/// a prompt to the cog's claude session. No DB gate (it's a nudge, not a hold); a no-live-cog match
/// is a benign no-op. Shares `live_sessions_for` with the control verbs.
fn send<H: SessionHost>(ledger: &Ledger, host: &H, target: &ControlTarget, message: &str) -> Result<()> {
    for s in live_sessions_for(ledger, target)? {
        let _ = host.send(&s.tmux_session, message);
    }
    Ok(())
}

/// Apply a control verb to its target, branching on the cog's shape. The DB gate differs — a
/// ticketed cog's hold is its own session, a captain's is the convoy, the lookout's is its singleton
/// row — but the session-reaching half (the graceful nudge, or the sink teardown) is uniform
/// (`signal`). The gate lives on the TRIGGER SOURCE so the cold cogs don't relaunch under anchor.
fn control<H: SessionHost>(ledger: &Ledger, host: &H, verb: Verb, target: &ControlTarget) -> Result<()> {
    match target {
        ControlTarget::Cog { cog, ticket } => control_cog(ledger, host, verb, *cog, ticket),
        ControlTarget::Captain { convoy } => control_captain(ledger, host, verb, convoy),
        ControlTarget::Lookout => control_lookout(ledger, host, verb),
    }
}

/// Worker/sailor/hand — the homogeneous ticketed surface, whose DB gate IS the session. anchor/weigh
/// set it on the live cog of that kind on the ticket; sink has no gate (the ended row blocks the
/// re-spawn). Then `signal` that same session. A no-live-cog match is a benign no-op.
fn control_cog<H: SessionHost>(ledger: &Ledger, host: &H, verb: Verb, cog: CrewKind, ticket: &TicketKey) -> Result<()> {
    for s in live_sessions_for(ledger, &ControlTarget::Cog { cog, ticket: ticket.clone() })? {
        match verb {
            Verb::Anchor => SessionRepo::anchor(ledger, &s.id, &now_rfc3339())?,
            Verb::Weigh => SessionRepo::weigh(ledger, &s.id, &now_rfc3339())?,
            Verb::Sink => {}
        }
        signal(ledger, host, verb, &s)?;
    }
    Ok(())
}

/// A captain — the hold lives on the CONVOY (the trigger source), not the cold session. anchor/sink
/// set the convoy gate (reconcile won't relaunch) + weigh clears it; a convoy with no live captain
/// still takes the gate (it holds the next launch). Then `signal` the live captain on that convoy.
fn control_captain<H: SessionHost>(ledger: &Ledger, host: &H, verb: Verb, convoy: &ConvoyKey) -> Result<()> {
    let now = now_rfc3339();
    match verb {
        Verb::Anchor | Verb::Sink => ConvoyRepo::anchor(ledger, convoy, &now)?,
        Verb::Weigh => ConvoyRepo::weigh(ledger, convoy, &now)?,
    }
    for s in live_sessions_for(ledger, &ControlTarget::Captain { convoy: convoy.clone() })? {
        signal(ledger, host, verb, &s)?;
    }
    Ok(())
}

/// The singleton lookout — the hold lives on its `lookout` row. anchor/sink set it + weigh clears
/// it; then `signal` the live lookout, if one is reviewing.
fn control_lookout<H: SessionHost>(ledger: &Ledger, host: &H, verb: Verb) -> Result<()> {
    let now = now_rfc3339();
    match verb {
        Verb::Anchor | Verb::Sink => LookoutRepo::anchor(ledger, &now)?,
        Verb::Weigh => LookoutRepo::weigh(ledger, &now)?,
    }
    for s in live_sessions_for(ledger, &ControlTarget::Lookout)? {
        signal(ledger, host, verb, &s)?;
    }
    Ok(())
}

/// The session-reaching half of a control verb, uniform across every target: anchor pushes the
/// graceful stop, weigh the resume nudge, sink ends the row + kills the tmux + logs the CDC (the
/// payload keyed off whichever of ticket/convoy the session carries). The DB gate is the caller's.
fn signal<H: SessionHost>(ledger: &Ledger, host: &H, verb: Verb, s: &Session) -> Result<()> {
    match verb {
        Verb::Anchor => {
            let _ = host.send(&s.tmux_session, ANCHOR_MSG);
        }
        Verb::Weigh => {
            let _ = host.send(&s.tmux_session, WEIGH_MSG);
        }
        Verb::Sink => {
            SessionRepo::end(ledger, &s.id, &now_rfc3339())?;
            let _ = host.kill(&s.tmux_session);
            ledger.append(
                &now_rfc3339(),
                EventKind::SessionEnded,
                EntityKind::Session,
                &s.id.0.to_string(),
                &serde_json::json!({
                    "ticket": s.ticket.as_ref().map(|k| &k.0),
                    "convoy": s.convoy.as_ref().map(|c| &c.0),
                    "kind": s.kind.as_str(),
                    "reason": "sink",
                }),
            )?;
        }
    }
    Ok(())
}

/// Resolve the live session for `(ticket, cog)` and record its report. No live session
/// = an anomaly (a cog reporting after it was reaped), surfaced as an error to the client.
fn record_status(
    ledger: &Ledger,
    cog: CrewKind,
    ticket: &TicketKey,
    state: CogState,
    last_action: Option<String>,
) -> Result<()> {
    let session = SessionRepo::for_ticket(ledger, ticket)?
        .into_iter()
        .find(|s| s.ended_at.is_none() && s.kind == cog)
        .ok_or_else(|| anyhow!("no live {} session for {}", cog.as_str(), ticket.0))?;
    ledger.record(&Report {
        session: session.id,
        state,
        last_action,
        reported_at: now_rfc3339(),
    })?;
    Ok(())
}

fn tick_liveness(ledger: &Ledger, host: &TmuxHost, snapshot: &Snapshot, thresholds: &LivenessThresholds) {
    match context(ledger, host, thresholds) {
        Ok(cogs) => *snapshot.lock().unwrap() = cogs,
        Err(e) => eprintln!("fleetd: liveness tick error: {e}"),
    }
}

/// Sync the board + armada, then record a `TickRan` event carrying the change-count
/// span (no-op detection) and the counts. Trait objects so it's testable with fakes.
pub fn tick_sync(ledger: &Ledger, forge: &dyn Forge, tracker: &dyn Tracker) -> Result<()> {
    let now = now_rfc3339();
    let before = ledger.total_changes();
    let tickets = sync::sync_board(ledger, tracker, &now)?;
    let ships = sync::sync_armada(ledger, forge, &now)?;
    let after = ledger.total_changes();
    ledger.append(
        &now,
        EventKind::TickRan,
        EntityKind::Ticker,
        "sync",
        &serde_json::json!({ "before": before, "after": after, "tickets": tickets, "ships": ships }),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::ports::EventLog;
    use domain::*;

    #[derive(Default)]
    struct FakeForge {
        disarmed: std::cell::RefCell<Vec<i64>>,
        armed: std::cell::RefCell<Vec<i64>>,
        drafted: std::cell::RefCell<Vec<(i64, bool)>>,
        opened: std::cell::RefCell<Vec<MrDraft>>,
        no_drafts: bool, // a forge plan without drafts
    }
    impl Forge for FakeForge {
        fn preflight(&self) -> Result<String> {
            Ok("fake".into())
        }
        fn stacks(&self) -> bool {
            true
        }
        fn armada(&self) -> Result<Vec<ForgeShip>> {
            Ok(vec![])
        }
        fn snapshot(&self, _: i64) -> Result<ForgeShip> {
            unimplemented!()
        }
        fn review_state(&self, _: i64) -> Result<ReviewState> {
            unimplemented!()
        }
        fn rebase(&self, _: i64) -> Result<()> {
            Ok(())
        }
        fn block_on(&self, _: i64, _: i64) -> Result<()> {
            Ok(())
        }
        fn arm_merge(&self, iid: i64) -> Result<()> {
            self.armed.borrow_mut().push(iid);
            Ok(())
        }
        fn disarm(&self, iid: i64) -> Result<()> {
            self.disarmed.borrow_mut().push(iid);
            Ok(())
        }
        fn set_draft(&self, iid: i64, draft: bool) -> Result<()> {
            if self.no_drafts {
                anyhow::bail!("drafts are not available on this repo");
            }
            self.drafted.borrow_mut().push((iid, draft));
            Ok(())
        }
        fn ci(&self, _: i64) -> Result<domain::CiReport> {
            unimplemented!()
        }
        fn retry_ci(&self, _: i64) -> Result<()> {
            Ok(())
        }
        fn reply(&self, _: i64, _: &str, _: &str, _: bool) -> Result<()> {
            Ok(())
        }
        fn open_mr(&self, d: &MrDraft) -> Result<String> {
            self.opened.borrow_mut().push(d.clone());
            Ok("https://forge/mr/1".into())
        }
        fn request_review(&self, _: &TicketKey) -> Result<Vec<String>> {
            Ok(vec![])
        }
    }

    struct FakeTracker(Vec<TrackerTicket>);
    impl Tracker for FakeTracker {
        fn preflight(&self) -> Result<String> {
            Ok("fake".into())
        }
        fn board(&self) -> Result<Vec<TrackerTicket>> {
            Ok(self.0.clone())
        }
        fn get(&self, key: &TicketKey) -> Result<TrackerTicket> {
            self.0.iter().find(|t| &t.key == key).cloned().ok_or_else(|| anyhow!("no such ticket"))
        }
        fn transition(&self, _: &TicketKey, _: TicketStatus) -> Result<()> {
            Ok(())
        }
        fn assigned_to_peer(&self, _: &TicketKey) -> Result<bool> {
            Ok(false)
        }
        fn claim(&self, _: &TicketKey) -> Result<()> {
            Ok(())
        }
        fn parent_of(&self, _: &TicketKey) -> Result<Option<TicketKey>> {
            Ok(None)
        }
        fn subtasks(&self, _: &TicketKey) -> Result<Vec<TicketStatus>> {
            Ok(vec![])
        }
        fn read(&self, _: &TicketKey) -> Result<domain::TicketDetail> {
            unimplemented!()
        }
        fn create(&self, _: &domain::TicketDraft) -> Result<TicketKey> {
            unimplemented!()
        }
        fn block(&self, _: &TicketKey, _: &TicketKey) -> Result<()> {
            Ok(())
        }
    }

    fn tt(key: &str, status: TicketStatus) -> TrackerTicket {
        TrackerTicket {
            key: TicketKey(key.into()),
            title: "T".into(),
            status,
            assignee: None,
            convoy: None,
            blocked_by: vec![],
        }
    }

    #[test]
    fn ensure_ticket_self_fetches_an_unsynced_ticket() {
        // An off-sprint ticket isn't in the ledger; ensure_ticket pulls it from the tracker so the
        // order FK holds.
        let db = Ledger::in_memory().unwrap();
        let key = TicketKey("PROJ-9".into());
        assert!(TicketRepo::get(&db, &key).unwrap().is_none(), "not synced yet");
        ensure_ticket(&db, &FakeTracker(vec![tt("PROJ-9", TicketStatus::Todo)]), &key).unwrap();
        assert_eq!(TicketRepo::get(&db, &key).unwrap().unwrap().status, Some(TicketStatus::Todo));
    }

    #[test]
    fn hold_disarms_and_drafts_the_open_mr_and_release_readies_it() {
        // An armed MR merges on the forge's own say — so a hold disarms it, clears `swept` so the
        // first pass after release re-sweeps (a fresh sailor re-arms), and drafts it.
        let db = Ledger::in_memory().unwrap();
        sync::sync_board(&db, &FakeTracker(vec![tt("PROJ-1", TicketStatus::InQa)]), "now").unwrap();
        let ship = Ship {
            id: ShipId(0),
            ticket: Some(TicketKey("PROJ-1".into())),
            iid: 7,
            url: "u".into(),
            target: "master".into(),
            source: String::new(),
            pilot: "me".into(),
            created_at: "t".into(),
            ended_at: None,
            swept: None,
            reviewed: None,
        };
        let sid = ShipRepo::upsert(&db, &ship).unwrap();
        db.set_swept(&sid, Some("fp")).unwrap();
        let forge = FakeForge::default();
        let key = TicketKey("PROJ-1".into());

        set_held(&db, &forge, &key, true, Some("late dependency")).unwrap();
        assert_eq!(*forge.disarmed.borrow(), vec![7], "the hold disarmed !7");
        assert_eq!(db.get_by_iid(7).unwrap().unwrap().swept, None, "swept cleared → re-sweep on release");
        assert!(TicketRepo::get(&db, &key).unwrap().unwrap().held);

        set_held(&db, &forge, &key, false, None).unwrap();
        assert_eq!(forge.disarmed.borrow().len(), 1, "release doesn't disarm");
        assert_eq!(*forge.drafted.borrow(), vec![(7, true), (7, false)], "hold drafts, release readies");
        assert!(!TicketRepo::get(&db, &key).unwrap().unwrap().held);

        // A forge without drafts: the hold + disarm stand, and the caller hears what failed.
        let no_drafts = FakeForge { no_drafts: true, ..Default::default() };
        let e = format!("{:#}", set_held(&db, &no_drafts, &key, true, None).unwrap_err());
        assert!(e.contains("held + disarmed, but couldn't mark !7 draft"), "{e}");
        assert!(TicketRepo::get(&db, &key).unwrap().unwrap().held, "the hold still stands");
        assert_eq!(*no_drafts.disarmed.borrow(), vec![7]);
    }

    #[test]
    fn launch_targets_the_explicit_base_else_the_order_fork_else_the_default_branch() {
        let dir = std::env::temp_dir().join(format!("fleetd-launch-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let git = |args: &[&str]| assert!(std::process::Command::new("git").arg("-C").arg(&dir).args(args).status().unwrap().success());
        git(&["init", "-q"]);
        git(&["checkout", "-q", "-b", "chore/PROJ-2-child"]);
        let worktree = dir.to_string_lossy().into_owned();

        let db = Ledger::in_memory().unwrap();
        sync::sync_board(&db, &FakeTracker(vec![tt("PROJ-2", TicketStatus::InProgress)]), "now").unwrap();
        let key = TicketKey("PROJ-2".into());
        OrderRepo::dispatch(&db, &Order {
            ticket: key.clone(),
            auto_impl: true,
            auto_merge: true,
            fork_from: "origin/chore/PROJ-1-parent".into(), // a stacked child
            ac_summary: "ac".into(),
            park_at: None,
            worktree_path: Some(worktree.clone()),
            created_at: "t".into(),
        })
        .unwrap();
        let forge = FakeForge::default();
        let req = |ticket: Option<TicketKey>, target: Option<&str>| Launch {
            ticket,
            worktree: worktree.clone(),
            target: target.map(String::from),
            default_branch: "origin/main".into(),
            title: "PROJ-2: child".into(),
            description: "why".into(),
            draft: false,
        };

        assert_eq!(launch(&db, &forge, req(Some(key.clone()), None)).unwrap(), "https://forge/mr/1");
        launch(&db, &forge, req(None, None)).unwrap();
        launch(&db, &forge, req(Some(key), Some("release"))).unwrap();
        let opened = forge.opened.borrow();
        let targets: Vec<_> = opened.iter().map(|d| (d.source.as_str(), d.target.as_str())).collect();
        assert_eq!(
            targets,
            [("chore/PROJ-2-child", "chore/PROJ-1-parent"), ("chore/PROJ-2-child", "main"), ("chore/PROJ-2-child", "release")],
            "order fork base, then the default branch, and an explicit target wins — origin/ stripped"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn arm_is_refused_while_the_ticket_is_held_or_blocked() {
        let db = Ledger::in_memory().unwrap();
        let mut child = tt("PROJ-2", TicketStatus::InQa);
        child.blocked_by = vec![TicketKey("PROJ-1".into())];
        sync::sync_board(&db, &FakeTracker(vec![tt("PROJ-1", TicketStatus::InProgress), child]), "now").unwrap();
        let ship = Ship {
            id: ShipId(0),
            ticket: Some(TicketKey("PROJ-2".into())),
            iid: 7,
            url: "u".into(),
            target: "main".into(),
            source: String::new(),
            pilot: "me".into(),
            created_at: "t".into(),
            ended_at: None,
            swept: None,
            reviewed: None,
        };
        ShipRepo::upsert(&db, &ship).unwrap();
        let (forge, key) = (FakeForge::default(), TicketKey("PROJ-2".into()));

        let e = arm(&db, &forge, &key).unwrap_err().to_string();
        assert!(e.contains("blocked by PROJ-1"), "{e}");

        sync::sync_board(&db, &FakeTracker(vec![tt("PROJ-1", TicketStatus::Done)]), "now").unwrap(); // the blocker lands
        TicketRepo::set_held(&db, &key, true, Some("late review"), "t").unwrap();
        assert!(arm(&db, &forge, &key).unwrap_err().to_string().contains("is held"));

        TicketRepo::set_held(&db, &key, false, None, "t").unwrap();
        arm(&db, &forge, &key).unwrap();
        assert_eq!(*forge.armed.borrow(), vec![7], "armed only once clear");
    }

    #[test]
    fn ensure_ticket_leaves_a_synced_ticket_untouched() {
        // Already in the ledger → no refetch (the empty tracker's get() would error if called).
        let db = Ledger::in_memory().unwrap();
        let key = TicketKey("PROJ-9".into());
        sync::sync_board(&db, &FakeTracker(vec![tt("PROJ-9", TicketStatus::InProgress)]), "now").unwrap();
        ensure_ticket(&db, &FakeTracker(vec![]), &key).unwrap();
        assert_eq!(TicketRepo::get(&db, &key).unwrap().unwrap().status, Some(TicketStatus::InProgress));
    }

    #[test]
    fn sync_tick_records_a_tickran_event_with_counts() {
        let db = Ledger::in_memory().unwrap();
        let forge = FakeForge::default();
        let tracker = FakeTracker(vec![TrackerTicket {
            key: TicketKey("PROJ-1".into()),
            title: "T".into(),
            status: TicketStatus::Todo,
            assignee: None,
            convoy: None,
            blocked_by: vec![],
        }]);

        tick_sync(&db, &forge, &tracker).unwrap();

        let events = db.since(Seq(0)).unwrap();
        let tick = events
            .iter()
            .find(|e| e.kind == EventKind::TickRan)
            .expect("a TickRan event");
        assert_eq!(tick.entity_kind, EntityKind::Ticker);
        assert_eq!(tick.entity_ref, "sync");
        assert_eq!(tick.payload["tickets"], 1);
        assert_eq!(tick.payload["ships"], 0);
        assert!(
            tick.payload["after"].as_u64().unwrap() >= tick.payload["before"].as_u64().unwrap(),
            "the change-count span must not go backwards"
        );
    }

    // ── anchor / weigh / sink ────────────────────────────────────────────────────────

    use std::cell::RefCell;
    use std::path::Path;

    /// Records kills + sends; the rest are harmless defaults.
    struct FakeHost {
        killed: RefCell<Vec<String>>,
        sent: RefCell<Vec<String>>,
    }
    impl FakeHost {
        fn new() -> Self {
            Self { killed: RefCell::new(vec![]), sent: RefCell::new(vec![]) }
        }
    }
    impl SessionHost for FakeHost {
        fn preflight(&self) -> Result<String> {
            Ok("fake".into())
        }
        fn spawn(&self, _: &str, _: &Path, _: &str) -> Result<()> {
            Ok(())
        }
        fn alive(&self, _: &str) -> Result<bool> {
            Ok(true)
        }
        fn last_activity(&self, _: &str) -> Result<Option<i64>> {
            Ok(None)
        }
        fn kill(&self, name: &str) -> Result<()> {
            self.killed.borrow_mut().push(name.into());
            Ok(())
        }
        fn send(&self, name: &str, _: &str) -> Result<()> {
            self.sent.borrow_mut().push(name.into());
            Ok(())
        }
        fn list(&self) -> Result<Vec<String>> {
            Ok(vec![])
        }
    }

    /// A ledger with one live worker on PROJ-1; returns (db, its session id).
    fn db_with_live_worker() -> (Ledger, SessionId) {
        use domain::ports::TicketRepo;
        let db = Ledger::in_memory().unwrap();
        TicketRepo::upsert(
            &db,
            &Ticket {
                key: TicketKey("PROJ-1".into()),
                convoy: None,
                held: false,
                created_at: "t".into(),
                blocked_by: vec![],
                title: None,
                assignee: None,
                status: None,
                synced_at: None,
            },
        )
        .unwrap();
        let id = SessionRepo::start(
            &db,
            &Session {
                id: SessionId(0),
                kind: CrewKind::Worker,
                ticket: Some(TicketKey("PROJ-1".into())),
                convoy: None,
                worktree_path: "/wt".into(),
                tmux_session: "worker-PROJ-1".into(),
                harness_session_id: None,
                created_at: "t".into(),
                ended_at: None,
                anchored_at: None,
            },
        )
        .unwrap()
        .unwrap();
        (db, id)
    }

    fn worker_target() -> ControlTarget {
        ControlTarget::Cog { cog: CrewKind::Worker, ticket: TicketKey("PROJ-1".into()) }
    }

    #[test]
    fn anchor_sets_the_flag_and_sends_but_never_kills() {
        let (db, id) = db_with_live_worker();
        let host = FakeHost::new();
        control(&db, &host, Verb::Anchor, &worker_target()).unwrap();
        assert!(SessionRepo::get(&db, &id).unwrap().unwrap().anchored_at.is_some());
        assert_eq!(host.sent.borrow().len(), 1, "a graceful stop signal is sent");
        assert!(host.killed.borrow().is_empty(), "anchor never kills");
    }

    #[test]
    fn send_pushes_to_the_live_cogs_pane_without_gating() {
        let (db, id) = db_with_live_worker();
        let host = FakeHost::new();
        send(&db, &host, &worker_target(), "handle the edge case").unwrap();
        assert_eq!(host.sent.borrow().as_slice(), &["worker-PROJ-1".to_string()], "reached the pane");
        assert!(host.killed.borrow().is_empty(), "send never kills");
        assert!(
            SessionRepo::get(&db, &id).unwrap().unwrap().anchored_at.is_none(),
            "send is a nudge, not a hold — no gate"
        );
    }

    #[test]
    fn sink_ends_the_session_and_kills_the_tmux() {
        let (db, id) = db_with_live_worker();
        let host = FakeHost::new();
        control(&db, &host, Verb::Sink, &worker_target()).unwrap();
        assert!(SessionRepo::get(&db, &id).unwrap().unwrap().ended_at.is_some(), "session ended");
        assert_eq!(host.killed.borrow().as_slice(), &["worker-PROJ-1".to_string()]);
        let ev = db.since(Seq(0)).unwrap();
        assert!(ev.iter().any(|e| e.kind == EventKind::SessionEnded && e.payload["reason"] == "sink"));
    }

    /// Start a ticketless cog (captain/lookout) directly and return its session id.
    fn start_ticketless(db: &Ledger, kind: CrewKind, convoy: Option<&str>, tmux: &str) -> SessionId {
        SessionRepo::start(
            db,
            &Session {
                id: SessionId(0),
                kind,
                ticket: None,
                convoy: convoy.map(|c| ConvoyKey(c.into())),
                worktree_path: "/project".into(),
                tmux_session: tmux.into(),
                harness_session_id: None,
                created_at: "t".into(),
                ended_at: None,
                anchored_at: None,
            },
        )
        .unwrap()
        .unwrap()
    }

    fn anchored_convoy(db: &Ledger, key: &str) -> bool {
        ConvoyRepo::active(db)
            .unwrap()
            .into_iter()
            .find(|c| c.key == ConvoyKey(key.into()))
            .map(|c| c.anchored_at.is_some())
            .unwrap_or(false)
    }

    #[test]
    fn captain_anchor_holds_the_convoy_gate_and_weigh_lifts_it() {
        let db = Ledger::in_memory().unwrap();
        ConvoyRepo::open(&db, &ConvoyKey("PROJ-100".into()), None, "t").unwrap();
        let host = FakeHost::new();
        let cap = ControlTarget::Captain { convoy: ConvoyKey("PROJ-100".into()) };
        // The hold lives on the convoy (the trigger source), with no live captain to touch.
        control(&db, &host, Verb::Anchor, &cap).unwrap();
        assert!(anchored_convoy(&db, "PROJ-100"), "anchor sets the convoy gate");
        assert!(host.killed.borrow().is_empty(), "no live captain → nothing scuttled");
        control(&db, &host, Verb::Weigh, &cap).unwrap();
        assert!(!anchored_convoy(&db, "PROJ-100"), "weigh lifts it");
    }

    #[test]
    fn captain_sink_anchors_the_convoy_and_scuttles_the_live_captain() {
        let db = Ledger::in_memory().unwrap();
        ConvoyRepo::open(&db, &ConvoyKey("PROJ-100".into()), None, "t").unwrap();
        let id = start_ticketless(&db, CrewKind::Captain, Some("PROJ-100"), "captain-PROJ-100");
        let host = FakeHost::new();
        control(&db, &host, Verb::Sink, &ControlTarget::Captain { convoy: ConvoyKey("PROJ-100".into()) }).unwrap();
        assert!(anchored_convoy(&db, "PROJ-100"), "sink holds the gate too (no relaunch)");
        assert!(SessionRepo::get(&db, &id).unwrap().unwrap().ended_at.is_some(), "captain session ended");
        assert_eq!(host.killed.borrow().as_slice(), &["captain-PROJ-100".to_string()]);
    }

    #[test]
    fn lookout_anchor_toggles_the_singleton_gate() {
        let db = Ledger::in_memory().unwrap();
        let host = FakeHost::new();
        control(&db, &host, Verb::Anchor, &ControlTarget::Lookout).unwrap();
        assert!(LookoutRepo::anchored(&db).unwrap(), "the lookout row holds the hold");
        control(&db, &host, Verb::Weigh, &ControlTarget::Lookout).unwrap();
        assert!(!LookoutRepo::anchored(&db).unwrap(), "weigh lifts it");
    }

    #[test]
    fn lookout_sink_anchors_and_scuttles_the_live_lookout() {
        let db = Ledger::in_memory().unwrap();
        let id = start_ticketless(&db, CrewKind::Lookout, None, "lookout");
        let host = FakeHost::new();
        control(&db, &host, Verb::Sink, &ControlTarget::Lookout).unwrap();
        assert!(LookoutRepo::anchored(&db).unwrap(), "sink holds the singleton gate");
        assert!(SessionRepo::get(&db, &id).unwrap().unwrap().ended_at.is_some(), "lookout session ended");
        assert_eq!(host.killed.borrow().as_slice(), &["lookout".to_string()]);
    }

    #[test]
    fn build_packet_composes_order_ship_snapshot_ticket() {
        use domain::ports::{OrderRepo, ShipRepo, TicketRepo};
        let db = Ledger::in_memory().unwrap();
        TicketRepo::upsert(
            &db,
            &Ticket {
                key: TicketKey("PROJ-1".into()),
                convoy: None,
                held: false,
                created_at: "t".into(),
                blocked_by: vec![],
                title: Some("T".into()),
                assignee: None,
                status: None,
                synced_at: None,
            },
        )
        .unwrap();
        OrderRepo::dispatch(
            &db,
            &Order {
                ticket: TicketKey("PROJ-1".into()),
                auto_impl: true,
                auto_merge: true,
                fork_from: "origin/master".into(),
                ac_summary: "do it".into(),
                park_at: None,
                worktree_path: Some("/wt".into()),
                created_at: "t".into(),
            },
        )
        .unwrap();
        let sid = ShipRepo::upsert(
            &db,
            &Ship {
                id: ShipId(0),
                ticket: Some(TicketKey("PROJ-1".into())),
                iid: 7,
                url: "u".into(),
                target: "master".into(),
                source: String::new(),
                pilot: "me".into(),
                created_at: "t".into(),
                ended_at: None,
                swept: None,
                reviewed: None,
            },
        )
        .unwrap();
        db.sync_snapshot(&ShipSnapshot {
            ship: sid,
            title: "T".into(),
            state: ShipState::Open,
            head_sha: "s".into(),
            has_conflicts: false,
            open_threads: 0,
            approvals: Approvals { current: 0, required: 0 },
            armed: false,
            on_merge_train: false,
            pipeline: None,
            merge_status: None,
            synced_at: "t".into(),
        })
        .unwrap();

        let p = build_packet(&db, &TicketKey("PROJ-1".into())).unwrap();
        assert_eq!(p.order.unwrap().ac_summary, "do it");
        assert_eq!(p.ship.unwrap().iid, 7);
        assert_eq!(p.snapshot.unwrap().state, ShipState::Open);
        assert_eq!(p.ticket.unwrap().key, TicketKey("PROJ-1".into()));
    }

    #[test]
    fn convoy_view_hands_a_stacked_ticket_its_parents_branch() {
        let db = Ledger::in_memory().unwrap();
        let convoy = ConvoyKey("PROJ-100".into());
        let tt = |key: &str, status: TicketStatus, blocked_by: Vec<&str>| TrackerTicket {
            key: TicketKey(key.into()),
            title: "T".into(),
            status,
            assignee: None,
            convoy: Some(convoy.clone()),
            blocked_by: blocked_by.into_iter().map(|b| TicketKey(b.into())).collect(),
        };
        let tickets = vec![tt("PROJ-1", TicketStatus::InQa, vec![]), tt("PROJ-2", TicketStatus::Todo, vec!["PROJ-1"])];
        sync::sync_board(&db, &FakeTracker(tickets), "now").unwrap();
        let parent = Ship {
            id: ShipId(0),
            ticket: Some(TicketKey("PROJ-1".into())),
            iid: 1,
            url: "u".into(),
            target: "main".into(),
            source: "feature/PROJ-1-parent".into(),
            pilot: "me".into(),
            created_at: "t".into(),
            ended_at: None,
            swept: None,
            reviewed: None,
        };
        ShipRepo::upsert(&db, &parent).unwrap();

        let view = build_convoy_view(&db, &convoy, true).unwrap();
        let child = view.tickets.iter().find(|t| t.key.0 == "PROJ-2").unwrap();
        assert_eq!(child.classification, Some(domain::Classification::Stacked));
        assert_eq!(child.fork_from.as_deref(), Some("origin/feature/PROJ-1-parent"));
        let view = build_convoy_view(&db, &convoy, false).unwrap();
        assert_eq!(view.tickets.iter().find(|t| t.key.0 == "PROJ-2").unwrap().fork_from, None, "no stacking, no fork base");
    }

    #[test]
    fn build_convoy_view_scopes_and_marks_ready_dispatched() {
        use domain::ports::TicketRepo;
        let db = Ledger::in_memory().unwrap();
        let t = |key: &str, convoy: &str, status: TicketStatus| Ticket {
            key: TicketKey(key.into()),
            convoy: Some(ConvoyKey(convoy.into())),
            held: false,
            created_at: "t".into(),
            blocked_by: vec![],
            title: None,
            assignee: None,
            status: Some(status),
            synced_at: None,
        };
        TicketRepo::upsert(&db, &t("PROJ-1", "PROJ-100", TicketStatus::Todo)).unwrap(); // ready, undispatched
        TicketRepo::upsert(&db, &t("PROJ-2", "PROJ-100", TicketStatus::Done)).unwrap(); // not ready
        TicketRepo::upsert(&db, &t("PROJ-9", "PROJ-200", TicketStatus::Todo)).unwrap(); // other convoy

        let view = build_convoy_view(&db, &ConvoyKey("PROJ-100".into()), true).unwrap();
        assert_eq!(view.tickets.len(), 2, "scoped to the convoy");
        let a = view.tickets.iter().find(|x| x.key == TicketKey("PROJ-1".into())).unwrap();
        assert_eq!(a.classification, Some(Classification::Clean), "To Do + unblocked");
        assert!(!a.dispatched, "no order");
        let b = view.tickets.iter().find(|x| x.key == TicketKey("PROJ-2".into())).unwrap();
        assert_eq!(b.classification, None, "Done is past the frontier");
        assert_eq!(b.status, Some(TicketStatus::Done));
    }

    #[test]
    fn control_target_from_wire_branches_on_cog_and_validates_its_key() {
        let ct = |cog: &str, ticket: Option<&str>, convoy: Option<&str>| wire::CogTarget {
            cog: cog.into(),
            ticket: ticket.map(Into::into),
            convoy: convoy.map(Into::into),
        };
        // worker/sailor/hand → a ticket.
        assert!(matches!(
            ControlTarget::from_wire(ct("worker", Some("PROJ-1"), None)),
            Ok(ControlTarget::Cog { cog: CrewKind::Worker, .. })
        ));
        assert!(ControlTarget::from_wire(ct("sailor", None, None)).is_err(), "needs a ticket");
        // captain → a convoy.
        assert!(matches!(
            ControlTarget::from_wire(ct("captain", None, Some("PROJ-100"))),
            Ok(ControlTarget::Captain { .. })
        ));
        assert!(ControlTarget::from_wire(ct("captain", None, None)).is_err(), "needs a convoy");
        // lookout → neither.
        assert!(matches!(ControlTarget::from_wire(ct("lookout", None, None)), Ok(ControlTarget::Lookout)));
        // unknown cog → a clear error, never a silent accept.
        assert!(ControlTarget::from_wire(ct("foreman", Some("PROJ-1"), None)).is_err());
    }
}
