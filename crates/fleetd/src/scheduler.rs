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
    ConvoyRepo, EventLog, Forge, LookoutRepo, OrderRepo, ProposalRepo, ReportRepo, SessionHost, SessionRepo,
    ShipRepo, TicketRepo, Tracker,
};
use domain::{
    CogState, ConvoyKey, ConvoyTicketView, ConvoyView, CrewKind, EntityKind, EventKind,
    LivenessThresholds, MrDraft, Order, Packet, Proposal, ProposalId, Proposed, Report, Session, Ship, ShipState, TicketDraft,
    TicketKey, TicketStatus,
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
/// A ship read's reply — the ship, its snapshot, and its fetch ref.
type ShipReply = std::sync::mpsc::Sender<std::result::Result<domain::ShipView, String>>;
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
    RecordStatus { cog: CrewKind, ticket: Option<TicketKey>, state: CogState, last_action: Option<String>, reply: Reply },
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
    Reply { cmd: wire::ReplyCmd, reply: Reply },
    GetShip { iid: i64, reply: ShipReply },           // a read: one ship + snapshot + fetch ref (ledger + forge)
    PostReview { cmd: wire::ReviewPostCmd, reply: Reply }, // an action: a summary review comment (+ approve) (forge) // an action: answer (and resolve) a review thread (forge)
    GetCi { iid: i64, reply: CiReply },         // a read: an MR's CI status + failed jobs (forge)
    RetryCi { iid: i64, reply: Reply },         // an action: re-run an MR's failed CI jobs (forge)
    Launch { launch: Launch, reply: UrlReply }, // an action: open a ticket's MR (forge)
    Arm { ticket: TicketKey, reply: Reply }, // an action: arm the ticket's MR, unless held/blocked (forge)
    GetTicket { ticket: TicketKey, reply: TicketReply }, // a read: a ticket's full text (tracker)
    NewTicket { draft: domain::TicketDraft, reply: UrlReply }, // an action: create a ticket → its key (tracker)
    BlockTicket { ticket: TicketKey, by: TicketKey, reply: Reply }, // an action: add an "is blocked by" edge (tracker)
    EditTicket { ticket: TicketKey, description: String, reply: Reply }, // an action: replace its description (tracker)
    CommentTicket { ticket: TicketKey, body: String, reply: Reply }, // an action: comment on it (tracker)
    Propose { from: Option<TicketKey>, proposed: Proposed, reply: UrlReply }, // hold a cog's proposal → its id
    AcceptProposal { id: ProposalId, convoy: Option<ConvoyKey>, reply: UrlReply }, // an action: accept it → its key (a new one is filed)
    DropProposal { id: ProposalId, reply: UrlReply },
    AddToConvoy { convoy: ConvoyKey, ticket: TicketKey, reply: UrlReply }, // → what it says (and what it left)
    RemoveFromConvoy { convoy: ConvoyKey, ticket: TicketKey, reply: UrlReply },
    Decommission { ticket: TicketKey, reason: String, reply: UrlReply }, // an action: close MR, cancel, reap → what it did
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
                tracing::error!("sync tick error: {e:#}");
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
                    tracing::info!("reconcile ({mode}) {n} actions");
                }
                Ok(_) => {}
                Err(e) => tracing::error!("reconcile tick error: {e:#}"),
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
            let res = record_status(ledger, cog, ticket.as_ref(), state, last_action)
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
        Command::GetShip { iid, reply } => {
            let _ = reply.send(ship_view(ledger, forge, iid).map_err(|e| format!("{e:#}")));
        }
        Command::PostReview { cmd, reply } => {
            let _ = reply.send(forge.post_review(cmd.iid, &cmd.body, cmd.approve).map_err(|e| format!("{e:#}")));
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
            let _ = reply.send(arm(ledger, forge, &ticket, cfg.forge.auto_merge).map_err(|e| format!("{e:#}")));
        }
        Command::GetTicket { ticket, reply } => {
            let _ = reply.send(read_ticket(ledger, tracker, &ticket).map_err(|e| format!("{e:#}")));
        }
        Command::NewTicket { draft, reply } => {
            let _ = reply.send(tracker.create(&draft).map(|k| k.0).map_err(|e| format!("{e:#}")));
        }
        Command::BlockTicket { ticket, by, reply } => {
            let _ = reply.send(tracker.block(&ticket, &by).map_err(|e| format!("{e:#}")));
        }
        Command::EditTicket { ticket, description, reply } => {
            let _ = reply.send(tracker.edit(&ticket, &description).map_err(|e| format!("{e:#}")));
        }
        Command::CommentTicket { ticket, body, reply } => {
            let _ = reply.send(tracker.comment(&ticket, &body).map_err(|e| format!("{e:#}")));
        }
        Command::Propose { from, proposed, reply } => {
            let res = propose(ledger, tracker, from, proposed)
                .map(|(id, key)| format!("proposal {}: {} — held for the operator (fleetc proposal {})", id.0, key.0, id.0));
            let _ = reply.send(res.map_err(|e| format!("{e:#}")));
        }
        Command::AcceptProposal { id, convoy, reply } => {
            let _ = reply.send(accept_proposal(ledger, tracker, &id, convoy).map(|k| k.0).map_err(|e| format!("{e:#}")));
        }
        Command::AddToConvoy { convoy, ticket, reply } => {
            let _ = reply.send(add_to_convoy(ledger, tracker, &convoy, &ticket).map_err(|e| format!("{e:#}")));
        }
        Command::RemoveFromConvoy { convoy, ticket, reply } => {
            let res = ConvoyRepo::remove_member(ledger, &convoy, &ticket);
            let _ = reply.send(res.map(|()| format!("{} left {} (back to its epic's convoy)", ticket.0, convoy.0)).map_err(|e| format!("{e:#}")));
        }
        Command::Decommission { ticket, reason, reply } => {
            let res = decommission(ledger, forge, tracker, host, &cfg.project.path, &ticket, &reason);
            let _ = reply.send(res.map_err(|e| format!("{e:#}")));
        }
        Command::DropProposal { id, reply } => {
            let _ = reply.send(drop_proposal(ledger, tracker, &id).map_err(|e| format!("{e:#}")));
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

/// The label on a ticket filed for a proposal, until the operator accepts it.
const PROPOSED: &str = "fleet-proposed";

/// Hold a cog's proposal. A new ticket is filed now, so the cog has a key to cite: labelled
/// `PROPOSED` and outside any convoy, so no captain dispatches it before it's accepted. It's then
/// held like an existing ticket. Either lands in the proposing ticket's convoy on accept unless it
/// names one; the ticket is pulled onto the board if it's off-sprint (the ledger's FK).
fn propose(ledger: &Ledger, tracker: &dyn Tracker, from: Option<TicketKey>, proposed: Proposed) -> Result<(ProposalId, TicketKey)> {
    let from_convoy = match &from {
        Some(t) => TicketRepo::get(ledger, t)?.ok_or_else(|| anyhow!("{} isn't on the board", t.0))?.convoy,
        None => None,
    };
    let (ticket, why, convoy) = match proposed {
        Proposed::New(d) => {
            let filed = TicketDraft { convoy: None, labels: vec![PROPOSED.into()], ..d.clone() };
            (tracker.create(&filed)?, d.title, d.convoy)
        }
        Proposed::Existing { ticket, why, convoy } => {
            anyhow::ensure!(from.as_ref() != Some(&ticket), "{} can't propose itself", ticket.0);
            (ticket, why, convoy)
        }
    };
    ensure_ticket(ledger, tracker, &ticket)?;
    let proposed = Proposed::Existing { ticket: ticket.clone(), why, convoy: convoy.or(from_convoy) };
    let p = Proposal { id: ProposalId(0), from_ticket: from, proposed, created_at: now_rfc3339(), ended_at: None, accepted_as: None };
    Ok((ledger.propose(&p)?, ticket))
}

/// The ticket was filed for a proposal and not yet accepted.
fn is_proposed(tracker: &dyn Tracker, key: &TicketKey) -> Result<bool> {
    Ok(tracker.read(key)?.labels.iter().any(|l| l == PROPOSED))
}

/// Comment `why` on the ticket and move it to Cancelled. The ledger learns it now, so the reap
/// never moves it to Done.
fn cancel(ledger: &Ledger, tracker: &dyn Tracker, ticket: &TicketKey, why: &str) -> Result<()> {
    tracker.comment(ticket, why)?;
    tracker.transition(ticket, TicketStatus::Cancelled)?;
    let mut t = tracker.get(ticket)?;
    t.status = TicketStatus::Cancelled;
    TicketRepo::sync(ledger, &t, &now_rfc3339())
}

/// Decommission a ticket: withdraw it from the fleet for good. Its open MRs are disarmed, told why,
/// and closed; `reason` goes on the ticket as a comment and it moves to Cancelled; then the reap ends
/// its cogs, removes its worktree and retires its order. Refused, before any change, once an MR of
/// it has merged. Each step is idempotent, so a run cut short is safe to repeat. Says what it did.
fn decommission<H: SessionHost>(
    ledger: &Ledger,
    forge: &dyn Forge,
    tracker: &dyn Tracker,
    host: &H,
    project: &str,
    ticket: &TicketKey,
    reason: &str,
) -> Result<String> {
    ensure_ticket(ledger, tracker, ticket)?;
    let mut ships = Vec::new();
    for ship in ledger.armada()?.into_iter().filter(|s| s.ticket.as_ref() == Some(ticket)) {
        let state = forge.snapshot(ship.iid)?.state;
        anyhow::ensure!(state != ShipState::Merged, "{}'s MR !{} has merged — nothing to decommission", ticket.0, ship.iid);
        ships.push((ship, state));
    }
    let now = now_rfc3339();
    let mut did = Vec::new();
    for (ship, state) in &ships {
        if matches!(state, ShipState::Open | ShipState::Draft) {
            forge.disarm(ship.iid)?; // before the close: an armed MR mustn't merge mid-way
            forge.post_review(ship.iid, &format!("Decommissioned: {reason}"), false)?;
            forge.close_mr(ship.iid)?;
            did.push(format!("closed !{}", ship.iid));
        }
        ShipRepo::retire(ledger, &ship.id, &now)?;
    }

    cancel(ledger, tracker, ticket, &format!("Decommissioned from the fleet: {reason}"))?;
    did.push(format!("{} → Cancelled (reason commented)", ticket.0));

    let cogs = SessionRepo::for_ticket(ledger, ticket)?.iter().filter(|s| s.ended_at.is_none()).count();
    let worktree = OrderRepo::get(ledger, ticket)?.and_then(|o| o.worktree_path);
    reconcile::reap(ledger, tracker, host, project, &ticket.0)?;
    if cogs > 0 {
        did.push(format!("ended {cogs} cog(s)"));
    }
    if let Some(wt) = worktree {
        did.push(format!("removed {wt}"));
    }
    ledger.append(
        &now,
        EventKind::TicketDecommissioned,
        EntityKind::Ticket,
        &ticket.0,
        &serde_json::json!({ "reason": reason, "closed": ships.iter().map(|(s, _)| s.iid).collect::<Vec<_>>() }),
    )?;
    Ok(did.join(" · "))
}

/// Put a ticket in a convoy — its only one (an off-sprint ticket is pulled onto the board first).
/// Says what happened, naming the epic's convoy it left, if any.
fn add_to_convoy(ledger: &Ledger, tracker: &dyn Tracker, convoy: &ConvoyKey, ticket: &TicketKey) -> Result<String> {
    ensure_ticket(ledger, tracker, ticket)?;
    Ok(match ledger.add_member(convoy, ticket, &now_rfc3339())? {
        Some(epic) => format!("{} is in convoy {} (moved from {}, its epic's)", ticket.0, convoy.0, epic.0),
        None => format!("{} is in convoy {}", ticket.0, convoy.0),
    })
}

fn open_proposal(ledger: &Ledger, id: &ProposalId) -> Result<Proposal> {
    ProposalRepo::get(ledger, id)?.filter(|p| p.ended_at.is_none()).ok_or_else(|| anyhow!("proposal {} isn't open", id.0))
}

/// Accept an open proposal and settle it as its ticket, landing it in its convoy (`convoy` when
/// given, else the one it carries). A ticket filed for it loses its `PROPOSED` label: it's official.
fn accept_proposal(ledger: &Ledger, tracker: &dyn Tracker, id: &ProposalId, convoy: Option<ConvoyKey>) -> Result<TicketKey> {
    let mut p = open_proposal(ledger, id)?;
    p.proposed.override_convoy(convoy);
    let key = match p.proposed {
        Proposed::New(draft) => tracker.create(&draft)?, // a draft held before proposals were filed on propose
        Proposed::Existing { ticket, convoy, .. } => {
            if is_proposed(tracker, &ticket)? {
                tracker.label(&ticket, PROPOSED, false)?;
            }
            if let Some(c) = &convoy {
                add_to_convoy(ledger, tracker, c, &ticket)?;
            }
            ticket
        }
    };
    ledger.settle(id, &now_rfc3339(), Some(&key))?;
    Ok(key)
}

/// Drop an open proposal. A ticket filed for it is cancelled; an existing one is left as it was.
fn drop_proposal(ledger: &Ledger, tracker: &dyn Tracker, id: &ProposalId) -> Result<String> {
    let p = open_proposal(ledger, id)?;
    let mut said = format!("dropped proposal {}", id.0);
    if let Proposed::Existing { ticket, .. } = &p.proposed {
        if is_proposed(tracker, ticket)? {
            cancel(ledger, tracker, ticket, &format!("Proposal {} dropped by the operator.", id.0))?;
            said.push_str(&format!(" — {} → Cancelled", ticket.0));
        }
    }
    ledger.settle(id, &now_rfc3339(), None)?;
    Ok(said)
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

/// One ship by iid — ours or an ally's — with its snapshot and the ref to fetch its head by.
fn ship_view(ledger: &Ledger, forge: &dyn Forge, iid: i64) -> Result<domain::ShipView> {
    let ship = ShipRepo::get_by_iid(ledger, iid)?.ok_or_else(|| anyhow!("!{iid}: not in the armada (synced yet?)"))?;
    let snapshot = ShipRepo::snapshot(ledger, &ship.id)?;
    Ok(domain::ShipView { ship, snapshot, fetch_ref: forge.fetch_ref(iid) })
}

/// A ticket's full text from the tracker, plus the fleet's hold on it from the ledger.
fn read_ticket(ledger: &Ledger, tracker: &dyn Tracker, key: &TicketKey) -> Result<domain::TicketDetail> {
    let mut detail = tracker.read(key)?;
    if TicketRepo::get(ledger, key)?.is_some_and(|t| t.held) {
        detail.held = Some(ledger.held_reason(key)?.unwrap_or_default());
    }
    Ok(detail)
}

/// The blockers of `ticket` not yet Done/Cancelled.
fn waiting_on(ledger: &Ledger, blockers: &[TicketKey]) -> Result<Vec<TicketKey>> {
    let mut open = Vec::new();
    for b in blockers {
        let status = TicketRepo::get(ledger, b)?.and_then(|t| t.status);
        if !matches!(status, Some(domain::TicketStatus::Done) | Some(domain::TicketStatus::Cancelled)) {
            open.push(b.clone());
        }
    }
    Ok(open)
}

/// Arm a ticket's open MR to merge once its gates pass. Refused while the ticket is held or has an
/// open blocker: the fleet's gates must hold at the moment the arm happens, not only when the sailor
/// launched (a hold or a blocker can land mid-sweep).
fn arm(ledger: &Ledger, forge: &dyn Forge, key: &TicketKey, merges: bool) -> Result<()> {
    let ticket = TicketRepo::get(ledger, key)?.ok_or_else(|| anyhow!("{}: not on the board", key.0))?;
    anyhow::ensure!(!ticket.held, "{} is held — not arming (fleetc unhold it first)", key.0);
    let open = reconcile::open_blockers(ledger, &ticket.blocked_by)?;
    if !open.is_empty() {
        let keys: Vec<&str> = open.iter().map(|b| b.0.as_str()).collect();
        anyhow::bail!("{} is blocked by {} — not arming until it lands", key.0, keys.join(", "));
    }
    anyhow::ensure!(merges, "{} is ready — this project merges by hand ([forge] auto_merge = false)", key.0);
    let ship = ledger
        .armada()?
        .into_iter()
        .find(|s| s.ticket.as_ref() == Some(key) && s.ended_at.is_none())
        .ok_or_else(|| anyhow!("{}: no open MR to arm", key.0))?;
    forge.arm_merge(ship.iid)?;
    refresh(ledger, forge, &ship);
    Ok(())
}

/// Re-read a ship the fleet just changed on the forge (armed, disarmed, drafted), so the ledger — and
/// the dashboard — show it now, not at the next sync. Best-effort: the sync catches up regardless.
fn refresh(ledger: &Ledger, forge: &dyn Forge, ship: &Ship) {
    if let Err(e) = reconcile::fresh_snapshot(ledger, forge, ship) {
        tracing::warn!("!{} not refreshed: {e:#}", ship.iid);
    }
}

/// Hold or release a ticket. An armed MR merges on the forge's own say, past every fleet gate, so a
/// hold also acts on the ticket's open MR: disarm it, clear its `swept` (the first pass after
/// release re-sweeps: a fresh sailor re-arms), and mark it draft — a
/// draft can't merge even if re-armed. Release marks it ready. Each forge step that fails is
/// reported with what already happened, so the caller can react (e.g. a forge without drafts).
fn set_held(ledger: &Ledger, forge: &dyn Forge, ticket: &TicketKey, held: bool, reason: Option<&str>) -> Result<()> {
    TicketRepo::set_held(ledger, ticket, held, reason, &now_rfc3339())?;
    if held {
        tracing::warn!("hold {}: {}", ticket.0, reason.unwrap_or("(no reason given)"));
    } else {
        tracing::info!("unhold {}", ticket.0);
    }
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
    refresh(ledger, forge, &ship);
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
        let waiting_on = waiting_on(ledger, &t.blocked_by)?;
        let held = if t.held { Some(ledger.held_reason(&t.key)?.unwrap_or_default()) } else { None };
        tickets.push(ConvoyTicketView {
            key: t.key,
            status: t.status,
            classification,
            dispatched,
            live_cog,
            ship_state,
            fork_from,
            waiting_on,
            held,
        });
    }
    Ok(ConvoyView { convoy: convoy.clone(), tickets })
}

/// Compose a cog's work-packet from the ledger: its dispatch order, its open ship + the ship's
/// latest snapshot, the cached ticket, and which of its blockers are still open. A pure read (the fresh-poll that keeps the snapshot
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
    let open_blockers = match &ticket_entity {
        Some(t) => reconcile::open_blockers(ledger, &t.blocked_by)?,
        None => vec![],
    };
    Ok(Packet { order, ship, snapshot, ticket: ticket_entity, open_blockers })
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

/// Push an operator message into the target cog's tmux pane(s), submitted as a prompt to its claude
/// session. No DB gate (it's a nudge, not a hold); a no-live-cog match is a benign no-op, but a pane
/// that won't take it is an error the operator sees. Shares `live_sessions_for` with the control verbs.
fn send<H: SessionHost>(ledger: &Ledger, host: &H, target: &ControlTarget, message: &str) -> Result<()> {
    for s in live_sessions_for(ledger, target)? {
        host.send(&s.tmux_session, message)?;
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
        // The hold is the ledger's; the nudge is best-effort (a dead pane can't take it).
        Verb::Anchor => {
            if let Err(e) = host.send(&s.tmux_session, ANCHOR_MSG) {
                tracing::warn!("anchor: nudge not delivered: {e:#}");
            }
        }
        Verb::Weigh => {
            if let Err(e) = host.send(&s.tmux_session, WEIGH_MSG) {
                tracing::warn!("weigh: nudge not delivered: {e:#}");
            }
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
    ticket: Option<&TicketKey>,
    state: CogState,
    last_action: Option<String>,
) -> Result<()> {
    // A ticket's cog is found by its ticket; without one, the live session of that kind (the lookout).
    let live = match ticket {
        Some(t) => SessionRepo::for_ticket(ledger, t)?.into_iter().filter(|s| s.ended_at.is_none()).collect(),
        None => SessionRepo::live(ledger)?,
    };
    let session = live.into_iter().find(|s| s.kind == cog).ok_or_else(|| {
        anyhow!("no live {} session{}", cog.as_str(), ticket.map(|t| format!(" for {}", t.0)).unwrap_or_default())
    })?;
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
        Err(e) => tracing::error!("liveness tick error: {e:#}"),
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
        closed: std::cell::RefCell<Vec<i64>>,
        noted: std::cell::RefCell<Vec<(i64, String)>>,
        state: Option<ShipState>, // what `snapshot` reports (None: Open)
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
        fn snapshot(&self, iid: i64) -> Result<ForgeShip> {
            let state = self.state.unwrap_or(ShipState::Open);
            Ok(ForgeShip {
                iid,
                url: "u".into(),
                target: "master".into(),
                source: String::new(),
                pilot: "me".into(),
                ticket: None,
                title: "T".into(),
                state,
                head_sha: "sha".into(),
                has_conflicts: false,
                open_threads: 0,
                approvals: Approvals { current: 0, required: 0 },
                armed: true,
                on_merge_train: false,
                pipeline: None,
                merge_status: None,
            })
        }
        fn review_state(&self, _: i64) -> Result<ReviewState> {
            unimplemented!()
        }
        fn rebase(&self, _: i64) -> Result<()> {
            Ok(())
        }
        fn sync_blocks(&self, _: i64, _: &[i64]) -> Result<usize> {
            Ok(0)
        }
        fn arm_merge(&self, iid: i64) -> Result<()> {
            self.armed.borrow_mut().push(iid);
            Ok(())
        }
        fn disarm(&self, iid: i64) -> Result<()> {
            self.disarmed.borrow_mut().push(iid);
            Ok(())
        }
        fn close_mr(&self, iid: i64) -> Result<()> {
            self.closed.borrow_mut().push(iid);
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
        fn fetch_ref(&self, iid: i64) -> String {
            format!("pull/{iid}/head")
        }
        fn post_review(&self, iid: i64, body: &str, _: bool) -> Result<()> {
            self.noted.borrow_mut().push((iid, body.into()));
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

    /// A tracker in memory: its tickets (created ones join) and their labels.
    struct FakeTracker {
        tickets: RefCell<Vec<TrackerTicket>>,
        labels: RefCell<Vec<(TicketKey, String)>>,
    }
    impl FakeTracker {
        fn new(tickets: Vec<TrackerTicket>) -> Self {
            Self { tickets: RefCell::new(tickets), labels: RefCell::new(vec![]) }
        }
    }
    impl Tracker for FakeTracker {
        fn preflight(&self) -> Result<String> {
            Ok("fake".into())
        }
        fn board(&self) -> Result<Vec<TrackerTicket>> {
            Ok(self.tickets.borrow().clone())
        }
        fn get(&self, key: &TicketKey) -> Result<TrackerTicket> {
            self.tickets.borrow().iter().find(|t| &t.key == key).cloned().ok_or_else(|| anyhow!("no such ticket"))
        }
        fn transition(&self, key: &TicketKey, to: TicketStatus) -> Result<()> {
            if let Some(t) = self.tickets.borrow_mut().iter_mut().find(|t| &t.key == key) {
                t.status = to;
            }
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
        fn read(&self, key: &TicketKey) -> Result<domain::TicketDetail> {
            let labels = self.labels.borrow().iter().filter(|(k, _)| k == key).map(|(_, l)| l.clone()).collect();
            Ok(domain::TicketDetail {
                ticket: self.get(key)?,
                kind: "task".into(),
                description: String::new(),
                held: None,
                location: String::new(),
                labels,
            })
        }
        fn create(&self, d: &TicketDraft) -> Result<TicketKey> {
            let key = TicketKey(format!("PROJ-{}", 100 + self.tickets.borrow().len()));
            self.tickets.borrow_mut().push(TrackerTicket {
                key: key.clone(),
                title: d.title.clone(),
                status: TicketStatus::Todo,
                assignee: None,
                convoy: d.convoy.clone(),
                blocked_by: d.blocked_by.clone(),
            });
            for l in &d.labels {
                self.label(&key, l, true)?;
            }
            Ok(key)
        }
        fn edit(&self, _: &TicketKey, _: &str) -> Result<()> {
            Ok(())
        }
        fn label(&self, key: &TicketKey, label: &str, on: bool) -> Result<()> {
            let mut labels = self.labels.borrow_mut();
            labels.retain(|(k, l)| !(k == key && l == label));
            if on {
                labels.push((key.clone(), label.into()));
            }
            Ok(())
        }
        fn block(&self, _: &TicketKey, _: &TicketKey) -> Result<()> {
            Ok(())
        }
        fn comment(&self, _: &TicketKey, _: &str) -> Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_new_proposal_is_filed_labelled_outside_its_convoy_and_made_official_on_accept() {
        let db = Ledger::in_memory().unwrap();
        let mut t = tt("PROJ-1", TicketStatus::InProgress);
        t.convoy = Some(ConvoyKey("PROJ-9".into()));
        let tracker = FakeTracker::new(vec![t]);
        sync::sync_board(&db, &tracker, "now").unwrap();
        let draft = TicketDraft { title: "Fix X".into(), kind: "bug".into(), description: "…".into(), convoy: None, blocked_by: vec![], labels: vec![] };
        let from = Some(TicketKey("PROJ-1".into()));
        let convoy_of = |k: &TicketKey| TicketRepo::get(&db, k).unwrap().unwrap().convoy;

        assert!(propose(&db, &tracker, Some(TicketKey("PROJ-404".into())), Proposed::New(draft.clone())).is_err(), "the proposing ticket must be on the board");
        assert_eq!(tracker.board().unwrap().len(), 1, "…checked before anything is filed");

        let id = propose(&db, &tracker, from, Proposed::New(draft)).unwrap().0;
        let Proposed::Existing { ticket: key, why, convoy } = ProposalRepo::get(&db, &id).unwrap().unwrap().proposed else { panic!("not filed") };
        assert_eq!((why.as_str(), convoy), ("Fix X", Some(ConvoyKey("PROJ-9".into()))), "lands in the proposing ticket's convoy on accept");
        assert!(is_proposed(&tracker, &key).unwrap(), "filed with the label");
        assert_eq!(convoy_of(&key), None, "outside any convoy: no captain dispatches it yet");

        assert_eq!(accept_proposal(&db, &tracker, &id, None).unwrap(), key);
        assert!(!is_proposed(&tracker, &key).unwrap(), "official: the label is gone");
        assert_eq!(convoy_of(&key), Some(ConvoyKey("PROJ-9".into())));
        assert!(db.pending().unwrap().is_empty());
        assert_eq!(ProposalRepo::get(&db, &id).unwrap().unwrap().accepted_as, Some(key));
        assert!(accept_proposal(&db, &tracker, &id, None).is_err(), "an accepted proposal can't be accepted again");
    }

    #[test]
    fn the_lookout_reports_without_a_ticket() {
        let db = Ledger::in_memory().unwrap();
        let lookout = Session {
            id: SessionId(0),
            kind: CrewKind::Lookout,
            ticket: None,
            convoy: None,
            worktree_path: "/project".into(),
            tmux_session: "lookout".into(),
            harness_session_id: None,
            created_at: "t".into(),
            ended_at: None,
            anchored_at: None,
        };
        let id = SessionRepo::start(&db, &lookout).unwrap().expect("session start");
        let reviewed = CogState::Lookout(LookoutState::Reviewed);
        assert!(record_status(&db, CrewKind::Worker, None, reviewed, None).is_err(), "no live worker");
        record_status(&db, CrewKind::Lookout, None, reviewed, None).unwrap();
        assert_eq!(ReportRepo::latest(&db, &id).unwrap().map(|r| r.state), Some(reviewed));
    }

    #[test]
    fn dropping_a_proposal_cancels_the_ticket_filed_for_it_but_never_an_existing_one() {
        let db = Ledger::in_memory().unwrap();
        let tracker = FakeTracker::new(vec![tt("PROJ-1", TicketStatus::InProgress), tt("PROJ-7", TicketStatus::Todo)]);
        sync::sync_board(&db, &tracker, "now").unwrap();
        let from = Some(TicketKey("PROJ-1".into()));
        let draft = TicketDraft { title: "Fix X".into(), kind: "bug".into(), description: "…".into(), convoy: None, blocked_by: vec![], labels: vec![] };
        let status = |k: &str| tracker.get(&TicketKey(k.into())).unwrap().status;

        let filed = propose(&db, &tracker, from.clone(), Proposed::New(draft)).unwrap().0;
        let said = drop_proposal(&db, &tracker, &filed).unwrap();
        assert!(said.contains("PROJ-102 → Cancelled"), "said: {said}");
        assert_eq!(status("PROJ-102"), TicketStatus::Cancelled);
        assert_eq!(TicketRepo::get(&db, &TicketKey("PROJ-102".into())).unwrap().unwrap().status, Some(TicketStatus::Cancelled));

        let existing = Proposed::Existing { ticket: TicketKey("PROJ-7".into()), why: "blocks the merge".into(), convoy: None };
        let id = propose(&db, &tracker, from, existing).unwrap().0;
        drop_proposal(&db, &tracker, &id).unwrap();
        assert_eq!(status("PROJ-7"), TicketStatus::Todo, "someone else's ticket is left as it was");
        assert!(db.pending().unwrap().is_empty());
        assert!(drop_proposal(&db, &tracker, &id).is_err(), "a dropped proposal can't be dropped again");
    }

    #[test]
    fn a_ticket_added_from_another_epic_is_in_our_convoys_view_not_its_epics() {
        let db = Ledger::in_memory().unwrap();
        let mut other = tt("PROJ-7", TicketStatus::Todo);
        other.convoy = Some(ConvoyKey("PROJ-50".into()));
        let tracker = FakeTracker::new(vec![other]);
        let (ours, epic) = (ConvoyKey("PROJ-90".into()), ConvoyKey("PROJ-50".into()));
        let key = TicketKey("PROJ-7".into());

        let said = add_to_convoy(&db, &tracker, &ours, &key).unwrap(); // off the board: pulled first
        assert!(said.contains("moved from PROJ-50"), "said: {said}");
        let in_view = |c: &ConvoyKey| build_convoy_view(&db, c, true).unwrap().tickets.iter().any(|t| t.key == key);
        assert!(in_view(&ours) && !in_view(&epic), "one convoy: ours");
        assert!(add_to_convoy(&db, &tracker, &ConvoyKey("PROJ-91".into()), &key).is_err(), "never two");
    }

    #[test]
    fn an_existing_ticket_lands_in_the_proposing_tickets_convoy_on_accept() {
        let db = Ledger::in_memory().unwrap();
        let mut from_ticket = tt("PROJ-1", TicketStatus::InQa);
        from_ticket.convoy = Some(ConvoyKey("PROJ-9".into()));
        let tracker = FakeTracker::new(vec![from_ticket.clone(), tt("PROJ-7", TicketStatus::Todo), tt("PROJ-8", TicketStatus::Todo)]);
        sync::sync_board(&db, &FakeTracker::new(vec![from_ticket]), "now").unwrap(); // PROJ-7/8 are off-sprint
        let from = Some(TicketKey("PROJ-1".into()));
        let existing = |k: &str, convoy: Option<&str>| Proposed::Existing {
            ticket: TicketKey(k.into()),
            why: "blocks the merge".into(),
            convoy: convoy.map(|c| ConvoyKey(c.into())),
        };
        let convoy_of = |k: &str| TicketRepo::get(&db, &TicketKey(k.into())).unwrap().unwrap().convoy;

        assert!(propose(&db, &tracker, from.clone(), existing("PROJ-1", None)).is_err(), "a ticket can't propose itself");
        let id = propose(&db, &tracker, from.clone(), existing("PROJ-7", None)).unwrap().0;
        assert_eq!(convoy_of("PROJ-7"), None, "pulled onto the board, in no convoy yet");
        assert_eq!(accept_proposal(&db, &tracker, &id, None).unwrap(), TicketKey("PROJ-7".into()), "nothing filed");
        assert_eq!(convoy_of("PROJ-7"), Some(ConvoyKey("PROJ-9".into())), "joined the proposing ticket's convoy");

        let named = propose(&db, &tracker, from, existing("PROJ-8", Some("PROJ-60"))).unwrap().0;
        accept_proposal(&db, &tracker, &named, Some(ConvoyKey("PROJ-61".into()))).unwrap();
        assert_eq!(convoy_of("PROJ-8"), Some(ConvoyKey("PROJ-61".into())), "the accepter's pick wins");
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
        ensure_ticket(&db, &FakeTracker::new(vec![tt("PROJ-9", TicketStatus::Todo)]), &key).unwrap();
        assert_eq!(TicketRepo::get(&db, &key).unwrap().unwrap().status, Some(TicketStatus::Todo));
    }

    #[test]
    fn hold_disarms_and_drafts_the_open_mr_and_release_readies_it() {
        // An armed MR merges on the forge's own say — so a hold disarms it, clears `swept` so the
        // first pass after release re-sweeps (a fresh sailor re-arms), and drafts it.
        let db = Ledger::in_memory().unwrap();
        sync::sync_board(&db, &FakeTracker::new(vec![tt("PROJ-1", TicketStatus::InQa)]), "now").unwrap();
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
        sync::sync_board(&db, &FakeTracker::new(vec![tt("PROJ-2", TicketStatus::InProgress)]), "now").unwrap();
        let key = TicketKey("PROJ-2".into());
        OrderRepo::dispatch(&db, &Order {
            ticket: key.clone(),
            auto_impl: true,
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
    fn arm_and_the_packet_wait_only_on_open_blockers() {
        let db = Ledger::in_memory().unwrap();
        let mut child = tt("PROJ-2", TicketStatus::InQa);
        child.blocked_by = vec![TicketKey("PROJ-1".into()), TicketKey("PROJ-3".into())];
        let board = vec![tt("PROJ-1", TicketStatus::InProgress), tt("PROJ-3", TicketStatus::Done), child];
        sync::sync_board(&db, &FakeTracker::new(board), "now").unwrap();
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

        let e = arm(&db, &forge, &key, true).unwrap_err().to_string();
        assert!(e.contains("blocked by PROJ-1 —"), "only the open blocker, not the landed PROJ-3: {e}");
        let open = |db: &Ledger| build_packet(db, &key).unwrap().open_blockers;
        assert_eq!(open(&db), vec![TicketKey("PROJ-1".into())], "the packet says what still gates the merge");

        sync::sync_board(&db, &FakeTracker::new(vec![tt("PROJ-1", TicketStatus::Done)]), "now").unwrap(); // the blocker lands
        assert!(open(&db).is_empty(), "every edge stays in ticket.blocked_by; none is open");
        TicketRepo::set_held(&db, &key, true, Some("late review"), "t").unwrap();
        assert!(arm(&db, &forge, &key, true).unwrap_err().to_string().contains("is held"));

        TicketRepo::set_held(&db, &key, false, None, "t").unwrap();
        let e = arm(&db, &forge, &key, false).unwrap_err().to_string();
        assert!(e.contains("merges by hand"), "{e}");
        arm(&db, &forge, &key, true).unwrap();
        assert_eq!(*forge.armed.borrow(), vec![7], "armed only once clear");
        let snap = ShipRepo::snapshot(&db, &db.armada().unwrap()[0].id).unwrap().unwrap();
        assert!(snap.armed, "the ledger shows the arm at once, not at the next sync");
    }

    #[test]
    fn ensure_ticket_leaves_a_synced_ticket_untouched() {
        // Already in the ledger → no refetch (the empty tracker's get() would error if called).
        let db = Ledger::in_memory().unwrap();
        let key = TicketKey("PROJ-9".into());
        sync::sync_board(&db, &FakeTracker::new(vec![tt("PROJ-9", TicketStatus::InProgress)]), "now").unwrap();
        ensure_ticket(&db, &FakeTracker::new(vec![]), &key).unwrap();
        assert_eq!(TicketRepo::get(&db, &key).unwrap().unwrap().status, Some(TicketStatus::InProgress));
    }

    #[test]
    fn sync_tick_records_a_tickran_event_with_counts() {
        let db = Ledger::in_memory().unwrap();
        let forge = FakeForge::default();
        let tracker = FakeTracker::new(vec![TrackerTicket {
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
    fn decommission_closes_the_mr_cancels_the_ticket_and_reaps_it() {
        let (db, id) = db_with_live_worker();
        let key = TicketKey("PROJ-1".into());
        let ship = |iid| Ship {
            id: ShipId(0),
            ticket: Some(key.clone()),
            iid,
            url: "u".into(),
            target: "master".into(),
            source: String::new(),
            pilot: "me".into(),
            created_at: "t".into(),
            ended_at: None,
            swept: None,
            reviewed: None,
        };
        ShipRepo::upsert(&db, &ship(7)).unwrap();
        let tracker = FakeTracker::new(vec![tt("PROJ-1", TicketStatus::InProgress)]);
        let host = FakeHost::new();

        let merged = FakeForge { state: Some(ShipState::Merged), ..Default::default() };
        assert!(decommission(&db, &merged, &tracker, &host, "/p", &key, "dup").is_err(), "a merged MR is refused");
        assert!(merged.closed.borrow().is_empty() && SessionRepo::get(&db, &id).unwrap().unwrap().ended_at.is_none(), "untouched");

        let forge = FakeForge { state: Some(ShipState::Open), ..Default::default() };
        let said = decommission(&db, &forge, &tracker, &host, "/p", &key, "superseded by PROJ-2").unwrap();
        assert_eq!(*forge.disarmed.borrow(), vec![7], "disarmed before the close");
        assert_eq!(*forge.closed.borrow(), vec![7]);
        assert_eq!(*forge.noted.borrow(), vec![(7, "Decommissioned: superseded by PROJ-2".to_string())]);
        assert_eq!(TicketRepo::get(&db, &key).unwrap().unwrap().status, Some(TicketStatus::Cancelled), "cancelled, not Done");
        assert!(SessionRepo::get(&db, &id).unwrap().unwrap().ended_at.is_some(), "its cog reaped");
        assert_eq!(host.killed.borrow().as_slice(), &["worker-PROJ-1".to_string()]);
        assert!(db.armada().unwrap().is_empty(), "the ship ended");
        assert!(said.contains("closed !7") && said.contains("Cancelled") && said.contains("ended 1 cog"), "said: {said}");
        assert!(db.since(Seq(0)).unwrap().iter().any(|e| e.kind == EventKind::TicketDecommissioned));
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
        sync::sync_board(&db, &FakeTracker::new(tickets), "now").unwrap();
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
        assert_eq!(child.waiting_on, vec![TicketKey("PROJ-1".into())], "PROJ-1 isn't done yet");

        TicketRepo::set_held(&db, &TicketKey("PROJ-2".into()), true, Some("ACs unclear"), "t").unwrap();
        let view = build_convoy_view(&db, &convoy, true).unwrap();
        let child = view.tickets.iter().find(|t| t.key.0 == "PROJ-2").unwrap();
        assert_eq!(child.held.as_deref(), Some("ACs unclear"), "a hold shows its reason");
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
