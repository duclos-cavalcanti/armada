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
use anyhow::{anyhow, bail, Context, Result};
use domain::ports::{
    CaseRepo, ConvoyRepo, EventLog, Forge, LookoutRepo, OrderRepo, ProposalRepo, ReportRepo, SessionHost, SessionRepo,
    InspectionRepo, ShipRepo, TicketRepo, Tracker,
};
use domain::{
    CogState, ConvoyKey, ConvoyTicketView, ConvoyView, CrewKind, EntityKind, EventKind, HandState, Inspection,
    CaseId, Ruling, TicketLabel, Verdict,
    LivenessThresholds, MrDraft, Order, Packet, Proposal, ProposalId, Proposed, Report, Session, Ship, ShipState, TicketDraft,
    TicketKey, TicketStatus, WorkerState,
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
/// A crossings read's reply — the MR under review beside our open tickets.
type CrossingsReply = std::sync::mpsc::Sender<std::result::Result<domain::Crossings, String>>;
/// A launch's reply — the new MR's URL.
type UrlReply = std::sync::mpsc::Sender<std::result::Result<String, String>>;
/// A settings-list read's reply — every live lever's effective value + source.
type SettingsReply = std::sync::mpsc::Sender<std::result::Result<Vec<crate::settings::LeverView>, String>>;

/// A write request from a client-facing thread (the server) to the single-writer
/// scheduler. Fields are already validated (parsed into `domain` types) — the lint
/// happened at the edge; this is the authoritative apply.
pub enum Command {
    RecordStatus { target: ControlTarget, state: CogState, last_action: Option<String>, reply: UrlReply }, // → "ok", or its case's note
    Dispatch { order: Order, reply: Reply },
    LaunchHand { worktree_path: String, reply: Reply }, // ticketless operator-driven hand, no order
    OpenConvoy { key: ConvoyKey, title: Option<String>, plan: Option<String>, goal: Option<TicketKey>, reply: Reply },
    CloseConvoy { key: ConvoyKey, reply: Reply },
    Anchor { target: ControlTarget, reply: Reply },
    Weigh { target: ControlTarget, reply: Reply },
    Sink { target: ControlTarget, reply: Reply },
    Send { target: ControlTarget, message: String, reply: Reply }, // push an operator message to the cog's pane
    AnchorTicket { ticket: TicketKey, anchor: bool, reason: Option<String>, reply: Reply }, // hold everything on it, or release it
    Rework { ticket: TicketKey, hand: bool, reply: Reply }, // send it back down the line to a builder
    ResetRounds { ticket: TicketKey, reply: Reply },         // its open MR's review rounds back to 0
    Inspect { ticket: TicketKey, verdict: Verdict, body: String, reply: Reply }, // a verdict on its cog's escalation
    Rule { case: Option<CaseId>, ticket: Option<TicketKey>, question: String, answer: String, reply: UrlReply }, // the operator's ruling → what it did
    GetPacket { ticket: TicketKey, reply: PacketReply }, // a read: the cog's composed work-packet
    GetConvoyView { convoy: ConvoyKey, reply: ConvoyReply }, // a read: the captain's convoy slice
    GetReview { iid: i64, reply: ReviewReply }, // a read: an MR's fresh review surface (forge)
    Rebase { iid: i64, reply: Reply },          // an action: server-side rebase an MR (forge)
    Reply { cmd: wire::ReplyCmd, reply: Reply },
    GetShip { iid: i64, reply: ShipReply },           // a read: one ship + snapshot + fetch ref (ledger + forge)
    GetCrossings { iid: i64, brief: bool, reply: CrossingsReply }, // a read: the MR beside our open tickets (ledger + forge + tracker)
    GetDiff { iid: i64, path: String, reply: UrlReply }, // a read: an MR's diff of one file (forge)
    PostReview { cmd: wire::ReviewPostCmd, reply: Reply }, // an action: a summary review comment (+ approve) (forge) // an action: answer (and resolve) a review thread (forge)
    GetCi { iid: i64, reply: CiReply },         // a read: an MR's CI status + failed jobs (forge)
    RetryCi { iid: i64, reply: Reply },         // an action: re-run an MR's failed CI jobs (forge)
    Launch { launch: Launch, reply: UrlReply }, // an action: open a ticket's MR (forge)
    Arm { ticket: TicketKey, reply: Reply }, // an action: arm the ticket's MR, unless held/blocked (forge)
    GetTicket { ticket: TicketKey, reply: TicketReply }, // a read: a ticket's full text (tracker)
    NewTicket { draft: domain::TicketDraft, reply: UrlReply }, // an action: create a ticket → its key (tracker)
    LinkTicket { ticket: TicketKey, other: TicketKey, link: domain::TicketLink, on: bool, captain: Option<ConvoyKey>, reply: Reply }, // an action: add or remove a link (tracker, + ledger)
    EditTicket { ticket: TicketKey, description: String, amend: Option<Amend>, reply: Reply }, // an action: replace its description (tracker)
    CommentTicket { ticket: TicketKey, body: String, reply: Reply }, // an action: comment on it (tracker)
    Propose { from: Option<TicketKey>, proposed: Proposed, reply: UrlReply }, // hold a cog's proposal → its id
    AcceptProposal { id: ProposalId, convoy: Option<ConvoyKey>, reply: UrlReply }, // an action: accept it → its key (a new one is filed)
    DropProposal { id: ProposalId, reply: UrlReply },
    AddToConvoy { convoy: ConvoyKey, ticket: TicketKey, hand: bool, reply: UrlReply }, // → what it says (and what it left)
    RemoveFromConvoy { convoy: ConvoyKey, ticket: TicketKey, reply: UrlReply },
    Decommission { ticket: TicketKey, reason: String, reply: UrlReply }, // an action: close MR, cancel, reap → what it did
    Recall { ticket: TicketKey, reason: String, weigh: bool, reply: UrlReply }, // an action: close MR, clear, back to the backlog, anchor (or weigh) → what it did
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
    let (mut next_poll, mut next_sync, mut next_reconcile, mut next_prune) =
        (Instant::now(), Instant::now(), Instant::now(), Instant::now());
    // The last health check (`None`: not run yet). Boot passed it; each sync re-runs it.
    let mut health: Option<Vec<domain::HealthCheck>> = None;

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
            if let Err(e) = tick_health(&ledger, &cfg, &mut health) {
                tracing::error!("health check error: {e:#}");
            }
            next_sync = now + Duration::from_secs(settings::sync_secs(&cfg, &ledger));
        }
        if now >= next_prune {
            match &cfg.project.prune {
                Some(_) if settings::dry_run(&cfg, &ledger) => tracing::info!("prune skipped (dry-run)"),
                Some(cmd) => drop(prune(cmd, &cfg.project.path)),
                None => {}
            }
            next_prune = now + PRUNE_EVERY;
        }
        if now >= next_reconcile {
            // Reconcile through the executor. dry_run (live) = plan + log, execute nothing; false =
            // apply through the ports (the Gate D flip). Reuse the liveness poll's memoized cogs (no
            // extra tmux fan-out).
            let dry_run = settings::dry_run(&cfg, &ledger);
            let levers = reconcile::Levers {
                dry_run,
                healthy: health.as_ref().is_none_or(|c| crate::health::problems(c).is_empty()),
                captain_sleep_secs: settings::captain_sleep_secs(&cfg, &ledger),
                admirals: settings::admirals(&cfg, &ledger),
                lookouts: settings::lookouts(&cfg, &ledger),
                self_lookouts: settings::self_lookouts(&cfg, &ledger),
                review_rounds: settings::review_rounds(&cfg, &ledger),
                review_settle_secs: settings::review_settle_secs(&cfg, &ledger),
                review_skill: cfg.review_skill.clone(),
                groom_skill: cfg.groom_skill.clone(),
                assess_skill: cfg.assess_skill.clone(),
                refused: Some(cfg.refused.clone()),
            };
            let cogs = snapshot.lock().unwrap().clone();
            match reconcile::tick(&ledger, tracker.as_ref(), &host, forge.as_ref(), &cogs, &cfg.project.path, &levers) {
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

/// Queue the order — a hand's for a hand-marked ticket, whatever the producer asked: whether a
/// ticket gets a hand is the ledger's call, not the captain's.
fn dispatch(ledger: &Ledger, order: Order) -> Result<()> {
    let auto_impl = order.auto_impl && !TicketRepo::is_hand(ledger, &order.ticket)?;
    ledger.dispatch(&Order { auto_impl, ..order })
}

fn apply(ledger: &Ledger, tracker: &dyn Tracker, host: &TmuxHost, forge: &dyn Forge, cfg: &Config, cmd: Command) {
    match cmd {
        Command::RecordStatus { target, state, last_action, reply } => {
            let res = record_status(ledger, forge, &target, state, last_action)
                .map_err(|e| e.to_string());
            let _ = reply.send(res); // receiver may have hung up; nothing to do
        }
        Command::Dispatch { order, reply } => {
            // Producer self-fetch: a lone / off-sprint ticket isn't synced, so the order FK
            // (orders.ticket → tickets.key) would fail. Pull it from the tracker + upsert first.
            // The worktree is NOT dispatch's job — the operating agent berths it (fleet-berth)
            // and passes it in the order; dispatch only ensures the ledger row exists.
            let res = ensure_ticket(ledger, tracker, &order.ticket)
                .and_then(|()| dispatch(ledger, order))
                .map_err(|e| e.to_string());
            let _ = reply.send(res);
        }
        Command::LaunchHand { worktree_path, reply } => {
            let res = reconcile::spawn_ticketless_hand(ledger, host, &worktree_path)
                .map_err(|e| e.to_string());
            let _ = reply.send(res);
        }
        Command::OpenConvoy { key, title, plan, goal, reply } => {
            let res = open_convoy(ledger, tracker, &key, title.as_deref(), plan.as_deref(), goal.as_ref()).map_err(|e| e.to_string());
            let _ = reply.send(res);
        }
        Command::CloseConvoy { key, reply } => {
            let res = close_convoy(ledger, &key).map_err(|e| format!("{e:#}"));
            let _ = reply.send(res);
        }
        Command::Anchor { target, reply } => {
            let res = control(ledger, host, Verb::Anchor, &target)
                .and_then(|()| hold_convoy(ledger, forge, host, &target, true))
                .map_err(|e| e.to_string());
            let _ = reply.send(res);
        }
        Command::Weigh { target, reply } => {
            let res = control(ledger, host, Verb::Weigh, &target)
                .and_then(|()| hold_convoy(ledger, forge, host, &target, false))
                .map_err(|e| e.to_string());
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
        Command::AnchorTicket { ticket, anchor, reason, reply } => {
            let res = anchor_ticket(ledger, forge, host, &ticket, anchor, reason.as_deref()).map_err(|e| format!("{e:#}"));
            let _ = reply.send(res);
        }
        Command::Rework { ticket, hand, reply } => {
            let res = rework(ledger, tracker, forge, host, &ticket, hand).map_err(|e| format!("{e:#}"));
            let _ = reply.send(res);
        }
        Command::ResetRounds { ticket, reply } => {
            let res = reset_rounds(ledger, &ticket).map_err(|e| format!("{e:#}"));
            let _ = reply.send(res);
        }
        Command::Inspect { ticket, verdict, body, reply } => {
            let res = inspect(ledger, &ticket, verdict, body).map_err(|e| format!("{e:#}"));
            let _ = reply.send(res);
        }
        Command::Rule { case, ticket, question, answer, reply } => {
            let res = rule(ledger, host, case, ticket, question, answer).map_err(|e| format!("{e:#}"));
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
        Command::GetCrossings { iid, brief, reply } => {
            let _ = reply.send(crate::crossings::crossings(ledger, tracker, forge, iid, brief).map_err(|e| format!("{e:#}")));
        }
        Command::GetDiff { iid, path, reply } => {
            let _ = reply.send(forge.file_diff(iid, &path).map_err(|e| format!("{e:#}")));
        }
        Command::PostReview { cmd, reply } => {
            let _ = reply.send(post_review(ledger, forge, &cmd).map_err(|e| format!("{e:#}")));
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
        Command::LinkTicket { ticket, other, link, on, captain, reply } => {
            let res = match &captain {
                Some(c) => in_convoy(ledger, c, &ticket).and_then(|()| in_convoy(ledger, c, &other)),
                None => Ok(()),
            };
            let res = res.and_then(|()| link_ticket(ledger, tracker, &ticket, &other, link, on));
            let _ = reply.send(res.map_err(|e| format!("{e:#}")));
        }
        Command::EditTicket { ticket, description, amend, reply } => {
            let res = match &amend {
                Some(a) => amend_ticket(ledger, tracker, &ticket, &description, a),
                None => tracker.edit(&ticket, &description),
            };
            let _ = reply.send(res.map_err(|e| format!("{e:#}")));
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
        Command::AddToConvoy { convoy, ticket, hand, reply } => {
            let res = add_to_convoy(ledger, tracker, &convoy, &ticket).and_then(|said| {
                TicketRepo::set_hand(ledger, &ticket, hand, &now_rfc3339())?;
                Ok(if hand { format!("{said}, for a hand") } else { said })
            });
            let _ = reply.send(res.map_err(|e| format!("{e:#}")));
        }
        Command::RemoveFromConvoy { convoy, ticket, reply } => {
            let res = ConvoyRepo::remove_member(ledger, &convoy, &ticket);
            let _ = reply.send(res.map(|()| format!("{} left {} (back to its epic's convoy)", ticket.0, convoy.0)).map_err(|e| format!("{e:#}")));
        }
        Command::Decommission { ticket, reason, reply } => {
            let res = decommission(ledger, forge, tracker, host, &cfg.project.path, &ticket, &reason);
            let _ = reply.send(res.map_err(|e| format!("{e:#}")));
        }
        Command::Recall { ticket, reason, weigh, reply } => {
            let res = recall(ledger, forge, tracker, host, &cfg.project.path, &ticket, &reason, weigh);
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

/// Hold a cog's proposal. A new ticket is filed now, so the cog has a key to cite: labelled
/// `TicketLabel::Proposed` and outside any convoy, so no captain dispatches it before it's accepted. It's then
/// held like an existing ticket. Either lands in the proposing ticket's convoy on accept unless it
/// names one; the ticket is pulled onto the board if it's off-sprint (the ledger's FK).
fn propose(ledger: &Ledger, tracker: &dyn Tracker, from: Option<TicketKey>, proposed: Proposed) -> Result<(ProposalId, TicketKey)> {
    let from_convoy = match &from {
        Some(t) => TicketRepo::get(ledger, t)?.ok_or_else(|| anyhow!("{} isn't on the board", t.0))?.convoy,
        None => None,
    };
    let (ticket, why, convoy) = match proposed {
        Proposed::New(d) => {
            let filed = TicketDraft { convoy: None, labels: vec![TicketLabel::Proposed.as_str().into()], ..d.clone() };
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

/// Close a convoy, unless an open proposal is still bound for it: accepting one would add a ticket to
/// a convoy already signed off. The operator settles them first.
fn close_convoy(ledger: &Ledger, key: &ConvoyKey) -> Result<()> {
    let open: Vec<String> = ProposalRepo::pending(ledger)?
        .into_iter()
        .filter(|p| p.proposed.convoy() == Some(key))
        .map(|p| match &p.proposed {
            Proposed::Existing { ticket, .. } => format!("#{} {}", p.id.0, ticket.0),
            Proposed::New(_) => format!("#{}", p.id.0),
        })
        .collect();
    anyhow::ensure!(open.is_empty(), "{} has open proposals: {}; accept or drop them first", key.0, open.join(", "));
    ledger.close(key, &now_rfc3339())
}

/// The ticket was filed for a proposal and not yet accepted.
fn is_proposed(tracker: &dyn Tracker, key: &TicketKey) -> Result<bool> {
    Ok(tracker.read(key)?.ticket.has(TicketLabel::Proposed))
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

/// Recall a ticket from the fleet and forget its work, without cancelling it: close its open MR
/// (noting why; its branch stays on the forge), end its cogs, discard its worktree and branch (unsaved
/// work and all) and its order, and return it to the backlog: To Do, unassigned, out of the sprint,
/// the reason commented. It stays anchored, so no captain dispatches it again until it's weighed;
/// `weigh` releases it at once (any hold on it too), its blocker edges alone gating its next dispatch.
/// Refused once its MR has merged: that work landed.
#[allow(clippy::too_many_arguments)]
fn recall<H: SessionHost>(
    ledger: &Ledger,
    forge: &dyn Forge,
    tracker: &dyn Tracker,
    host: &H,
    project: &str,
    ticket: &TicketKey,
    reason: &str,
    weigh: bool,
) -> Result<String> {
    ensure_ticket(ledger, tracker, ticket)?;
    let mut ships = Vec::new();
    for ship in ledger.armada()?.into_iter().filter(|s| s.ticket.as_ref() == Some(ticket)) {
        let state = forge.snapshot(ship.iid)?.state;
        anyhow::ensure!(state != ShipState::Merged, "{}'s MR !{} has merged — nothing to recall", ticket.0, ship.iid);
        ships.push((ship, state));
    }
    let now = now_rfc3339();
    let mut did = Vec::new();
    for (ship, state) in &ships {
        if matches!(state, ShipState::Open | ShipState::Draft) {
            forge.disarm(ship.iid)?; // before the close: an armed MR mustn't merge mid-way
            forge.post_review(ship.iid, &format!("Recalled to the backlog: {reason}"), false)?;
            forge.close_mr(ship.iid)?;
            did.push(format!("closed !{}", ship.iid));
        }
        ShipRepo::retire(ledger, &ship.id, &now)?;
    }

    let cogs = SessionRepo::for_ticket(ledger, ticket)?.iter().filter(|s| s.ended_at.is_none()).count();
    let worktree = OrderRepo::get(ledger, ticket)?.and_then(|o| o.worktree_path);
    reconcile::clear(ledger, host, project, &ticket.0, true)?;
    if cogs > 0 {
        did.push(format!("ended {cogs} cog(s)"));
    }
    if let Some(wt) = worktree {
        did.push(format!("discarded {wt}"));
    }

    // Anchored before the tracker moves, so a captain's pass in between can't dispatch it again;
    // weighed only once it's back in To Do, so a failure on the way leaves it anchored.
    TicketRepo::set_held(ledger, ticket, true, Some(&format!("recalled: {reason}")), &now)?;
    match tracker.unclaim(ticket) {
        Ok(()) => did.push(format!("{} → To Do, unassigned, out of the sprint", ticket.0)),
        Err(e) => did.push(format!("{}: {e:#}", ticket.0)),
    }
    tracker.comment(ticket, &format!("Recalled from the fleet to the backlog: {reason}"))?;
    TicketRepo::sync(ledger, &tracker.get(ticket)?, &now)?; // the board shows it back in To Do now
    if weigh {
        TicketRepo::set_held(ledger, ticket, false, None, &now)?;
        did.push("weighed: dispatched again once its blockers clear".into());
    } else {
        did.push("anchored (weigh to release)".into());
    }
    ledger.append(
        &now,
        EventKind::TicketRecalled,
        EntityKind::Ticket,
        &ticket.0,
        &serde_json::json!({ "reason": reason, "weighed": weigh, "closed": ships.iter().map(|(s, _)| s.iid).collect::<Vec<_>>() }),
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
/// given, else the one it carries). A ticket filed for it loses its `TicketLabel::Proposed`: it's official.
fn accept_proposal(ledger: &Ledger, tracker: &dyn Tracker, id: &ProposalId, convoy: Option<ConvoyKey>) -> Result<TicketKey> {
    let mut p = open_proposal(ledger, id)?;
    p.proposed.override_convoy(convoy);
    let key = match p.proposed {
        Proposed::New(draft) => tracker.create(&draft)?, // a draft held before proposals were filed on propose
        Proposed::Existing { ticket, convoy, .. } => {
            if is_proposed(tracker, &ticket)? {
                tracker.label(&ticket, TicketLabel::Proposed.as_str(), false)?;
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
/// A ticket whose MR is already open (a rework relaunching) gets it rewritten, not a second one.
fn launch(ledger: &Ledger, forge: &dyn Forge, l: Launch) -> Result<String> {
    if let Some(ship) = l.ticket.as_ref().map(|k| open_ship(ledger, k)).transpose()?.flatten() {
        forge.describe_mr(ship.iid, &l.title, &l.description)?;
        return Ok(ship.url);
    }
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
/// Post a lookout's review — a summary comment, or one finding as a resolvable thread — approving
/// only an ally's MR: our own merge on the team's approvals, never on the fleet's own say.
fn post_review(ledger: &Ledger, forge: &dyn Forge, cmd: &wire::ReviewPostCmd) -> Result<()> {
    if cmd.thread {
        return forge.post_thread(cmd.iid, &cmd.body);
    }
    if cmd.approve {
        let ours = match ShipRepo::get_by_iid(ledger, cmd.iid)? {
            Some(ship) => reconcile::is_ours(ledger, &ship)?,
            None => false,
        };
        anyhow::ensure!(!ours, "!{} is ours — a lookout never approves our own MR (post without --approve)", cmd.iid);
    }
    forge.post_review(cmd.iid, &cmd.body, cmd.approve)
}

fn ship_view(ledger: &Ledger, forge: &dyn Forge, iid: i64) -> Result<domain::ShipView> {
    let ship = ShipRepo::get_by_iid(ledger, iid)?.ok_or_else(|| anyhow!("!{iid}: not in the armada (synced yet?)"))?;
    let snapshot = ShipRepo::snapshot(ledger, &ship.id)?;
    let ours = reconcile::is_ours(ledger, &ship)?;
    Ok(domain::ShipView { ship, snapshot, fetch_ref: forge.fetch_ref(iid), ours })
}

/// A ticket's full text from the tracker, plus the fleet's hold on it from the ledger.
fn read_ticket(ledger: &Ledger, tracker: &dyn Tracker, key: &TicketKey) -> Result<domain::TicketDetail> {
    let mut detail = tracker.read(key)?;
    if TicketRepo::get(ledger, key)?.is_some_and(|t| t.held) {
        detail.held = Some(ledger.held_reason(key)?.unwrap_or_default());
    }
    Ok(detail)
}

/// Open a convoy, or re-open one: its tickets synced, then its title and plan set. On a convoy that
/// already exists, an omitted title or plan keeps its own, so re-opening to change one never
/// blanks the other.
fn open_convoy(
    ledger: &Ledger,
    tracker: &dyn Tracker,
    key: &ConvoyKey,
    title: Option<&str>,
    plan: Option<&str>,
    goal: Option<&TicketKey>,
) -> Result<()> {
    let now = now_rfc3339();
    sync::sync_convoy(ledger, tracker, key, &now)?;
    ledger.open(key, title, &now)?;
    if let Some(plan) = plan {
        ledger.set_plan(key, Some(plan), &now)?;
    }
    match goal {
        Some(goal) => ledger.set_goal(key, goal, &now),
        None => Ok(()),
    }
}

/// A captain's amendment of a ticket in its convoy: why, and the escalation case it settles, if any.
#[derive(Debug, Clone)]
pub struct Amend {
    pub captain: ConvoyKey,
    pub why: String,
    pub case: Option<CaseId>,
}

/// A captain changes only its own convoy's tickets: `ticket` must be in `convoy`.
fn in_convoy(ledger: &Ledger, convoy: &ConvoyKey, ticket: &TicketKey) -> Result<()> {
    let theirs = TicketRepo::get(ledger, ticket)?.and_then(|t| t.convoy);
    anyhow::ensure!(
        theirs.as_ref() == Some(convoy),
        "{} isn't in convoy {}: a captain changes only its own convoy's tickets — escalate it",
        ticket.0,
        convoy.0
    );
    Ok(())
}

/// A captain amends a ticket in its convoy: the description is replaced, and the change goes on the
/// ticket as a comment (the diff, why, the case it settles) and in the ledger, so the operator can
/// review it afterwards and reverse it with a ruling.
fn amend_ticket(ledger: &Ledger, tracker: &dyn Tracker, ticket: &TicketKey, description: &str, a: &Amend) -> Result<()> {
    in_convoy(ledger, &a.captain, ticket)?;
    let before = tracker.read(ticket)?.description;
    tracker.edit(ticket, description)?;
    let diff = similar::TextDiff::from_lines(before.trim_end(), description.trim_end()).unified_diff().context_radius(2).to_string();
    let case = a.case.as_ref().map(|c| format!(", case #{}", c.0)).unwrap_or_default();
    let note = format!("Amended by the captain of convoy {}{case}: {}\n\n```diff\n{}```", a.captain.0, a.why.trim(), diff);
    tracker.comment(ticket, &note)?;
    ledger.append(
        &now_rfc3339(),
        EventKind::TicketAmended,
        EntityKind::Ticket,
        &ticket.0,
        &serde_json::json!({ "captain": a.captain.0, "case": a.case.as_ref().map(|c| c.0), "why": a.why, "diff": diff }),
    )?;
    Ok(())
}

/// Add or remove a link between two tickets in the tracker. A blocking edge goes into the ledger
/// too, so the line holds (or frees) the ticket now, not at the next sync.
fn link_ticket(ledger: &Ledger, tracker: &dyn Tracker, key: &TicketKey, other: &TicketKey, link: domain::TicketLink, on: bool) -> Result<()> {
    tracker.link(key, other, link, on)?;
    if link == domain::TicketLink::BlockedBy {
        if let Some(t) = TicketRepo::get(ledger, key)? {
            let mut blockers = t.blocked_by;
            blockers.retain(|b| b != other);
            if on {
                blockers.push(other.clone());
            }
            ledger.set_blocked_by(key, &blockers)?;
        }
    }
    Ok(())
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
    anyhow::ensure!(!ticket.held, "{} is anchored — not arming (fleetc weigh it first)", key.0);
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
/// The ticket's open MR, if it has one.
fn open_ship(ledger: &Ledger, ticket: &TicketKey) -> Result<Option<Ship>> {
    Ok(ledger.armada()?.into_iter().find(|s| s.ticket.as_ref() == Some(ticket) && s.ended_at.is_none()))
}

/// Keep an MR from merging: disarm it, clear `swept` so the next sweep re-arms, and draft it.
fn hold_ship(ledger: &Ledger, forge: &dyn Forge, ship: &Ship) -> Result<()> {
    let iid = ship.iid;
    forge.disarm(iid).with_context(|| format!("disarming !{iid} failed"))?;
    ShipRepo::set_swept(ledger, &ship.id, None)?;
    forge.set_draft(iid, true).with_context(|| format!("disarmed, but couldn't mark !{iid} draft"))
}

/// Record a verdict on the open case of the ticket's escalated worker or sailor. A re-report of the
/// same question stays judged; a new question is a new case, uninspected until it's judged in turn.
fn inspect(ledger: &Ledger, ticket: &TicketKey, verdict: Verdict, body: String) -> Result<()> {
    let (session, report) = reconcile::escalation(ledger, ticket)?
        .ok_or_else(|| anyhow!("{} has no escalated worker or sailor to inspect", ticket.0))?;
    let case = ledger.open_case(&session.id)?.ok_or_else(|| anyhow!("{}: its escalation has no open case", ticket.0))?;
    let i = Inspection { session: session.id, case: Some(case.id), reported_at: report.reported_at, verdict, body, inspected_at: now_rfc3339() };
    InspectionRepo::inspect(ledger, &i)
}

/// Rule on a confirmed case: record the ruling, then hand it to the cog still escalated on it, so no
/// ruling reaches a cog unrecorded. With no case, a ruling with no escalation behind it, on `ticket`.
fn rule<H: SessionHost>(
    ledger: &Ledger,
    host: &H,
    case: Option<CaseId>,
    ticket: Option<TicketKey>,
    question: String,
    answer: String,
) -> Result<String> {
    anyhow::ensure!(!question.trim().is_empty() && !answer.trim().is_empty(), "a ruling has a question and an answer");
    let ruling = Ruling { question: question.trim().into(), answer: answer.trim().into(), ruled_at: now_rfc3339() };
    let Some(id) = case else {
        let ticket = ticket.ok_or_else(|| anyhow!("a ruling names its case, or the ticket it's on"))?;
        let id = ledger.rule_alone(&ticket, &ruling)?;
        return Ok(format!("case #{}: ruled on {}", id.0, ticket.0));
    };
    ledger.rule(&id, &ruling)?;
    let case = ledger.case(&id)?.ok_or_else(|| anyhow!("case #{} vanished", id.0))?;
    let live = match (&case.session, &case.settled_at) {
        (Some(s), None) => SessionRepo::get(ledger, s)?.filter(|s| s.ended_at.is_none()),
        _ => None,
    };
    let Some(cog) = live else {
        return Ok(format!("case #{}: ruled; its cog has moved on, so it's recorded only", id.0));
    };
    // One line: the pane takes a newline as Enter. The ruling in full is on the case, and so is the
    // verdict's prepared part, which goes with it: the cog gets the whole solution at once.
    let answer = ruling.answer.split_whitespace().collect::<Vec<_>>().join(" ");
    let prepared = ledger.verdict(&id)?.and_then(|v| v.prepared()).is_some();
    let then = if prepared { "carry it out with the verdict's prepared steps, as this ruling amends them" } else { "carry it out" };
    let msg = format!(
        "⚖ Ruling on your escalation (case #{}): {answer} — {then}, then report your state. In full: fleetc escalations --case {}",
        id.0, id.0
    );
    host.send(&cog.tmux_session, &msg).with_context(|| format!("case #{}: ruled, but not delivered to {}", id.0, cog.tmux_session))?;
    Ok(format!("case #{}: ruled and sent to {}", id.0, cog.tmux_session))
}

/// Reset the review rounds of the ticket's open MR: back to 0, so its sailors sweep on past the cap
/// (`sailor.md`'s ten). The operator's call, or the admiral's on their ruling; fleetc refuses a cog.
fn reset_rounds(ledger: &Ledger, ticket: &TicketKey) -> Result<()> {
    let ship = ShipRepo::armada(ledger)?
        .into_iter()
        .find(|s| s.ticket.as_ref() == Some(ticket) && s.ended_at.is_none())
        .ok_or_else(|| anyhow!("{} has no open MR", ticket.0))?;
    ShipRepo::reset_rounds(ledger, &ship.id, &now_rfc3339())?;
    Ok(())
}

/// Rework a ticket whose ACs changed under its MR: send it back down the line. A worker re-enters
/// at To Do (its spawn claims it back, once the blockers are Done); a hand starts at once, wherever
/// the ticket is. Its live cogs are sunk and its MR held as a draft; the order is re-dated, so
/// reconcile launches the builder in the same worktree, on the same branch and MR.
fn rework<H: SessionHost>(
    ledger: &Ledger,
    tracker: &dyn Tracker,
    forge: &dyn Forge,
    host: &H,
    ticket: &TicketKey,
    hand: bool,
) -> Result<()> {
    let hand = hand || TicketRepo::is_hand(ledger, ticket)?;
    OrderRepo::get(ledger, ticket)?.ok_or_else(|| anyhow!("{} was never dispatched: fleetc dispatch it", ticket.0))?;
    let status = TicketRepo::get(ledger, ticket)?.and_then(|t| t.status);
    if let Some(s @ (TicketStatus::Done | TicketStatus::Cancelled)) = status {
        anyhow::bail!("{} is {} — nothing to rework", ticket.0, s.as_str());
    }
    if !hand {
        tracker.transition(ticket, TicketStatus::Todo)?; // first: a refused move changes nothing
        TicketRepo::sync(ledger, &tracker.get(ticket)?, &now_rfc3339())?;
    }
    hold_sessions(ledger, host, Verb::Sink, live_cogs_on(ledger, ticket)?)?;
    if let Some(ship) = open_ship(ledger, ticket)? {
        hold_ship(ledger, forge, &ship)?;
        refresh(ledger, forge, &ship);
    }
    OrderRepo::rework(ledger, ticket, !hand, &now_rfc3339())
}

fn refresh(ledger: &Ledger, forge: &dyn Forge, ship: &Ship) {
    if let Err(e) = reconcile::fresh_snapshot(ledger, forge, ship) {
        tracing::warn!("!{} not refreshed: {e:#}", ship.iid);
    }
}

/// Anchor a ticket — hold everything on it, whatever its stage — or weigh it (release it all). Each
/// stop applies where its target exists: the ticket is held (off the frontier; `fleetc arm`
/// refuses), its live cogs are anchored, and its open MR is disarmed + drafted. An armed MR merges
/// on the forge's own say, past every fleet gate, hence the disarm; its `swept` clears so the first
/// pass after release re-sweeps; a draft can't merge even if re-armed. Weighing readies the MR
/// only in the sailor's phase (In QA): a worker's MR was a draft before the anchor and stays one
/// until it concludes. Then it resumes the cogs. A forge step that fails is reported with what already happened, so the
/// caller can react (e.g. a forge without drafts).
fn anchor_ticket<H: SessionHost>(
    ledger: &Ledger,
    forge: &dyn Forge,
    host: &H,
    ticket: &TicketKey,
    anchor: bool,
    reason: Option<&str>,
) -> Result<()> {
    TicketRepo::set_held(ledger, ticket, anchor, reason, &now_rfc3339())?;
    // The ticket's working cogs — not its admiral, the operator's own seat.
    let working = || -> Result<Vec<Session>> {
        Ok(live_cogs_on(ledger, ticket)?.into_iter().filter(|s| s.kind != CrewKind::Admiral).collect())
    };
    if anchor {
        tracing::warn!("anchor {}: {}", ticket.0, reason.unwrap_or("(no reason given)"));
        hold_sessions(ledger, host, Verb::Anchor, working()?)?;
    } else {
        tracing::info!("weigh {}", ticket.0);
    }
    if let Some(ship) = open_ship(ledger, ticket)? {
        if anchor {
            hold_ship(ledger, forge, &ship).context("anchored")?;
        } else if TicketRepo::get(ledger, ticket)?.and_then(|t| t.status) == Some(TicketStatus::InQa) {
            let iid = ship.iid;
            forge.set_draft(iid, false).with_context(|| format!("weighed, but couldn't mark !{iid} ready"))?;
        }
        refresh(ledger, forge, &ship);
    }
    if !anchor {
        hold_sessions(ledger, host, Verb::Weigh, working()?)?;
    }
    Ok(())
}

/// Compose a captain's convoy slice: every ticket in the convoy, each marked `ready` (fleetd's
/// `is_ready` frontier) + `dispatched` (an order exists), plus a light cog/ship awareness. The
/// captain dispatches `ready && !dispatched` and closes the convoy once every ticket is terminal.
fn build_convoy_view(ledger: &Ledger, convoy: &ConvoyKey, stacks: bool) -> Result<ConvoyView> {
    let armada = ShipRepo::armada(ledger)?;
    let board = TicketRepo::board(ledger)?;
    let goal = ConvoyRepo::goal(ledger, convoy)?;
    let path = goal.as_ref().map(|g| domain::upstream(g, &board)).unwrap_or_default();
    let mut tickets = Vec::new();
    for t in board {
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
        // A STACKED ticket forks off its parent's branch.
        let fork_from = crate::reconcile::stack_base(ledger, &t, &armada, stacks)?;
        let waiting_on = waiting_on(ledger, &t.blocked_by)?;
        let held = if t.held { Some(ledger.held_reason(&t.key)?.unwrap_or_default()) } else { None };
        let hand = TicketRepo::is_hand(ledger, &t.key)?;
        let inspect = match crate::reconcile::escalation(ledger, &t.key)? {
            Some((s, _)) => match ledger.open_case(&s.id)? {
                Some(case) => ledger.verdict(&case.id)?.is_none(),
                None => true,
            },
            None => false,
        };
        tickets.push(ConvoyTicketView {
            critical: path.contains(&t.key),
            key: t.key,
            status: t.status,
            classification,
            dispatched,
            live_cog,
            ship_state,
            fork_from,
            waiting_on,
            held,
            hand,
            inspect,
        });
    }
    Ok(ConvoyView { convoy: convoy.clone(), plan: ConvoyRepo::plan(ledger, convoy)?, goal, tickets })
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
/// (§A): worker/sailor/hand/admiral are addressed by `ticket` (+ kind), a captain by its `convoy`, a
/// lookout by its MR's `iid` (none: every lookout). Parsed + validated from the wire `CogTarget`.
pub enum ControlTarget {
    Cog { cog: CrewKind, ticket: TicketKey }, // the homogeneous ticketed surface
    Captain { convoy: ConvoyKey },
    Lookout { iid: Option<i64> },
}

impl ControlTarget {
    /// Validate the wire DTO: `cog` must be a known kind, and its key must match — ticketed cogs need
    /// a `ticket`, a captain a `convoy`, the lookout neither (a mismatch is a clear error at the edge).
    pub fn from_wire(t: wire::CogTarget) -> std::result::Result<Self, String> {
        let cog = CrewKind::parse(&t.cog).ok_or_else(|| format!("unknown --cog {:?}", t.cog))?;
        if t.iid.is_some() && cog != CrewKind::Lookout {
            return Err(format!("--iid targets a lookout, not a {}", cog.as_str()));
        }
        match cog {
            CrewKind::Worker | CrewKind::Sailor | CrewKind::Hand | CrewKind::Admiral => {
                let ticket = t.ticket.ok_or_else(|| format!("--cog {} needs --ticket", cog.as_str()))?;
                let ticket = TicketKey::parse(&ticket).map_err(|e| e.to_string())?;
                Ok(ControlTarget::Cog { cog, ticket })
            }
            CrewKind::Captain => {
                let convoy = t.convoy.ok_or_else(|| "--cog captain needs --convoy".to_string())?;
                let convoy = ConvoyKey::parse(&convoy).map_err(|e| e.to_string())?;
                Ok(ControlTarget::Captain { convoy })
            }
            CrewKind::Lookout => Ok(ControlTarget::Lookout { iid: t.iid }),
        }
    }

    pub fn kind(&self) -> CrewKind {
        match self {
            ControlTarget::Cog { cog, .. } => *cog,
            ControlTarget::Captain { .. } => CrewKind::Captain,
            ControlTarget::Lookout { .. } => CrewKind::Lookout,
        }
    }

    /// The key for a message: ` for <ticket>`, ` for <convoy>`, or nothing (the lookout).
    fn key(&self) -> String {
        match self {
            ControlTarget::Cog { ticket, .. } => format!(" for {}", ticket.0),
            ControlTarget::Captain { convoy } => format!(" for {}", convoy.0),
            ControlTarget::Lookout { iid: Some(iid) } => format!(" on !{iid}"),
            ControlTarget::Lookout { iid: None } => String::new(),
        }
    }
}

/// The live session(s) a control/send verb targets — the ONE resolution shared by anchor/weigh/sink
/// and send. Worker/sailor/hand/admiral resolve by (ticket, kind); a captain by its convoy; a lookout
/// by its ship (every lookout, when none is named).
fn live_sessions_for(ledger: &Ledger, target: &ControlTarget) -> Result<Vec<Session>> {
    Ok(match target {
        ControlTarget::Cog { cog, ticket } => {
            live_cogs_on(ledger, ticket)?.into_iter().filter(|s| s.kind == *cog).collect()
        }
        ControlTarget::Captain { convoy } => SessionRepo::live(ledger)?
            .into_iter()
            .filter(|s| s.kind == CrewKind::Captain && s.convoy.as_ref() == Some(convoy))
            .collect(),
        ControlTarget::Lookout { iid } => {
            let ship = match iid {
                Some(iid) => Some(ledger.get_by_iid(*iid)?.ok_or_else(|| anyhow!("no MR !{iid}"))?.id),
                None => None,
            };
            SessionRepo::live(ledger)?
                .into_iter()
                .filter(|s| s.kind == CrewKind::Lookout && ship.as_ref().is_none_or(|id| s.ship.as_ref() == Some(id)))
                .collect()
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
/// ticketed cog's hold is its own session, a captain's is the convoy, the lookouts' is their one
/// control row — but the session-reaching half (the graceful nudge, or the sink teardown) is uniform
/// (`signal`). The gate lives on the TRIGGER SOURCE so the cold cogs don't relaunch under anchor.
fn control<H: SessionHost>(ledger: &Ledger, host: &H, verb: Verb, target: &ControlTarget) -> Result<()> {
    match target {
        ControlTarget::Cog { cog, ticket } => control_cog(ledger, host, verb, *cog, ticket),
        ControlTarget::Captain { convoy } => control_captain(ledger, host, verb, convoy),
        ControlTarget::Lookout { iid } => control_lookout(ledger, host, verb, *iid),
    }
}

/// Worker/sailor/hand — the homogeneous ticketed surface, whose DB gate IS the session. anchor/weigh
/// set it on the live cog of that kind on the ticket; sink has no gate (the ended row blocks the
/// re-spawn). Then `signal` that same session. A no-live-cog match is a benign no-op.
fn control_cog<H: SessionHost>(ledger: &Ledger, host: &H, verb: Verb, cog: CrewKind, ticket: &TicketKey) -> Result<()> {
    hold_sessions(ledger, host, verb, live_sessions_for(ledger, &ControlTarget::Cog { cog, ticket: ticket.clone() })?)
}

/// Apply a control verb to ticketed sessions, whose DB gate IS the session: anchor/weigh set or clear
/// it, sink has none (the ended row blocks the re-spawn); then `signal` each.
fn hold_sessions<H: SessionHost>(ledger: &Ledger, host: &H, verb: Verb, sessions: Vec<Session>) -> Result<()> {
    for s in sessions {
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

/// A convoy anchor holds the whole convoy, not only its captain: every open ticket in it is anchored
/// as `fleetc anchor <KEY>` would (its cogs politely interrupted, its MR disarmed and drafted), with
/// the convoy's reason. Its weigh releases exactly those: a ticket already anchored for a reason of
/// its own is left as it is, either way. Best-effort per ticket: one that fails is named, the rest
/// still hold. A no-op for any target but a captain.
fn hold_convoy<H: SessionHost>(ledger: &Ledger, forge: &dyn Forge, host: &H, target: &ControlTarget, anchor: bool) -> Result<()> {
    let ControlTarget::Captain { convoy } = target else { return Ok(()) };
    let reason = format!("convoy {} anchored", convoy.0);
    let mut failed = Vec::new();
    for t in TicketRepo::board(ledger)?.into_iter().filter(|t| t.convoy.as_ref() == Some(convoy)) {
        if matches!(t.status, Some(TicketStatus::Done) | Some(TicketStatus::Cancelled)) {
            continue;
        }
        let ours = ledger.held_reason(&t.key)?.as_deref() == Some(reason.as_str());
        // Anchor what isn't held yet; weigh only what this anchor held.
        let ours_to_move = if anchor { !t.held } else { t.held && ours };
        if !ours_to_move {
            continue;
        }
        if let Err(e) = anchor_ticket(ledger, forge, host, &t.key, anchor, anchor.then_some(reason.as_str())) {
            failed.push(format!("{}: {e:#}", t.key.0));
        }
    }
    anyhow::ensure!(failed.is_empty(), "{} — the convoy's other tickets are {}", failed.join("; "), if anchor { "anchored" } else { "weighed" });
    Ok(())
}

/// The lookouts — their hold lives on the `lookout` row, over them all. anchor/sink set it + weigh
/// clears it; then `signal` every live lookout. Sinking ONE (its MR's `iid`) ends just that review:
/// its MR is marked reviewed, so it waits for its next push.
fn control_lookout<H: SessionHost>(ledger: &Ledger, host: &H, verb: Verb, iid: Option<i64>) -> Result<()> {
    let now = now_rfc3339();
    match (verb, iid) {
        (Verb::Anchor | Verb::Weigh, Some(_)) => bail!("anchor / weigh hold every lookout: drop --iid"),
        (Verb::Sink, Some(_)) => {}
        (Verb::Anchor | Verb::Sink, None) => LookoutRepo::anchor(ledger, &now)?,
        (Verb::Weigh, None) => LookoutRepo::weigh(ledger, &now)?,
    }
    for s in live_sessions_for(ledger, &ControlTarget::Lookout { iid })? {
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
/// = an anomaly (a cog reporting after it was reaped), surfaced as an error to the client. Replies
/// `ok`, or, to an escalation, what it did to the cog's case: raised a new one, or joined the one
/// still awaiting its answer (so the cog waits, rather than escalate again).
fn record_status(
    ledger: &Ledger,
    forge: &dyn Forge,
    target: &ControlTarget,
    state: CogState,
    last_action: Option<String>,
) -> Result<String> {
    let session = live_sessions_for(ledger, target)?
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("no live {} session{}", target.kind().as_str(), target.key()))?;
    // Nothing goes to a sailor while a blocker is open: it would only be reworked once that merges.
    // A hand's `done` closes the ticket outright: same gate, and only with no MR to land.
    let done = state == CogState::Hand(HandState::Done);
    if let (true, ControlTarget::Cog { ticket, .. }) = (state.is_concluded() || done, target) {
        let blockers = TicketRepo::get(ledger, ticket)?.map(|t| t.blocked_by).unwrap_or_default();
        let open = crate::reconcile::open_blockers(ledger, &blockers)?;
        if !open.is_empty() {
            let keys: Vec<&str> = open.iter().map(|k| k.0.as_str()).collect();
            bail!("{} is blocked by {} (not merged) — it can't conclude until they merge", ticket.0, keys.join(", "));
        }
        if done && open_ship(ledger, ticket)?.is_some() {
            bail!("{} has an open MR — yield it to a sailor (/fleet-yield), not done", ticket.0);
        }
    }
    // A worker's MR goes up for review, and concludes only reviewed: a lookout reviewed its current
    // head, no lookout is still on it, and no thread is open. Fresh-polled, so a push just made
    // counts: `review` records the head the lookout will review.
    if let (CogState::Worker(s @ (WorkerState::Review | WorkerState::Concluded)), ControlTarget::Cog { ticket, .. }) = (state, target) {
        let ship = open_ship(ledger, ticket)?
            .ok_or_else(|| anyhow!("{} has no open MR — open its draft first (/fleet-launch)", ticket.0))?;
        let snap = reconcile::fresh_snapshot(ledger, forge, &ship)?;
        if s == WorkerState::Concluded {
            let iid = ship.iid;
            let reviewed = ship.reviewed.as_deref() == Some(snap.head_sha.as_str()) && !reconcile::lookout_on(ledger, &ship.id)?;
            anyhow::ensure!(reviewed, "!{iid} isn't reviewed at its head — report review, and conclude once the lookout's review is clean");
            let open = snap.open_threads;
            anyhow::ensure!(open == 0, "!{iid} has {open} open review threads — answer them (/fleet-parley), then report review");
        }
    }
    let before = ledger.open_case(&session.id)?.map(|c| c.id);
    ledger.record(&Report {
        session: session.id.clone(),
        state,
        last_action,
        reported_at: now_rfc3339(),
    })?;
    let Some(case) = ledger.open_case(&session.id)?.filter(|_| state.is_escalated()) else {
        return Ok("ok".into());
    };
    if before.as_ref() == Some(&case.id) {
        return Ok(format!(
            "case #{} still awaits its answer, so this report joined it: nothing else changes. Wait in your pane: \
             the answer comes to you (a REFUTE's way out, or ⚖ the operator's ruling).",
            case.id.0
        ));
    }
    Ok(format!("case #{} raised: a captain inspects it. Wait in your pane for the answer.", case.id.0))
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

/// How often fleetd prunes the checkout's build caches (`[project] prune`): once a day, and at boot.
const PRUNE_EVERY: Duration = Duration::from_secs(24 * 60 * 60);

/// Run the project's prune command in its checkout, off the scheduler's thread: the line never
/// waits on it. Its outcome is logged, its last line of output with it.
fn prune(cmd: &str, checkout: &str) -> std::thread::JoinHandle<()> {
    let (cmd, checkout) = (cmd.to_string(), checkout.to_string());
    std::thread::spawn(move || {
        let out = std::process::Command::new("sh").args(["-c", &cmd]).current_dir(&checkout).stdin(std::process::Stdio::null()).output();
        let last = |b: &[u8]| String::from_utf8_lossy(b).lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").to_string();
        match out {
            Ok(o) if o.status.success() => tracing::info!("prune: {cmd}: {}", last(&o.stdout).trim()),
            Ok(o) => tracing::warn!("prune: {cmd} failed ({}): {}", o.status, last(&o.stderr).trim()),
            Err(e) => tracing::warn!("prune: {cmd} didn't run: {e}"),
        }
    })
}

/// Re-run the health check after a sync. A change (or the first run) is recorded as a
/// `health_checked` event, every named check in it, which the dashboard's `fleet:` light and Health
/// tab read, and logged: a failure loudly, as it holds the lookouts until it passes.
fn tick_health(ledger: &Ledger, cfg: &Config, last: &mut Option<Vec<domain::HealthCheck>>) -> Result<()> {
    let checks = crate::health::checks(cfg);
    if last.as_ref() == Some(&checks) {
        return Ok(());
    }
    let problems = crate::health::problems(&checks);
    match problems.as_slice() {
        [] => tracing::info!("health: ok"),
        misses => tracing::error!("health: {} — lookouts held until it passes", misses.join("; ")),
    }
    let warns: Vec<String> = checks
        .iter()
        .filter(|c| c.status == domain::HealthStatus::Warn)
        .map(|c| format!("{}: {}", c.name, c.detail.as_deref().unwrap_or("")))
        .collect();
    if !warns.is_empty() {
        tracing::warn!("health: {}", warns.join("; "));
    }
    ledger.append(
        &now_rfc3339(),
        EventKind::HealthChecked,
        EntityKind::Ticker,
        "health",
        &serde_json::json!({ "ok": problems.is_empty(), "problems": problems, "checks": checks }),
    )?;
    *last = Some(checks);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    const LEVERS: reconcile::Levers = reconcile::Levers {
        dry_run: false,
        healthy: true,
        captain_sleep_secs: 0,
        admirals: 0,
        lookouts: 1,
        self_lookouts: -1,
        review_rounds: 5,
        review_settle_secs: 0,
        review_skill: None,
        groom_skill: None,
        assess_skill: None,
        refused: None,
    };
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
        threads: i64,             // the open review threads `snapshot` reports
        paths: Vec<(i64, Vec<String>)>, // what `changed_paths` reports
    }
    impl Forge for FakeForge {
        fn preflight(&self) -> Result<String> {
            Ok("fake".into())
        }
        fn changed_paths(&self, iids: &[i64]) -> Result<Vec<(i64, Vec<String>)>> {
            Ok(self.paths.iter().filter(|(i, _)| iids.contains(i)).cloned().collect())
        }
        fn stacks(&self) -> bool {
            true
        }

        fn armada_is_exact(&self) -> bool {
            false
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
                open_threads: self.threads,
                total_threads: 0,
                approvals: Approvals { current: 0, required: 0 },
                armed: true,
                on_merge_train: false,
                blocked_by: vec![],
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
        fn describe_mr(&self, iid: i64, title: &str, _: &str) -> Result<()> {
            self.noted.borrow_mut().push((iid, title.into()));
            Ok(())
        }
        fn request_review(&self, _: &TicketKey) -> Result<Vec<String>> {
            Ok(vec![])
        }
    }

    /// A tracker in memory: its tickets (created ones join) and their labels.
    struct FakeTracker {
        tickets: RefCell<Vec<TrackerTicket>>,
        labels: RefCell<Vec<(TicketKey, String)>>,
        comments: RefCell<Vec<(TicketKey, String)>>,
    }
    impl FakeTracker {
        fn new(tickets: Vec<TrackerTicket>) -> Self {
            Self { tickets: RefCell::new(tickets), labels: RefCell::new(vec![]), comments: RefCell::new(vec![]) }
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
        fn children(&self, parent: &TicketKey) -> Result<Vec<TrackerTicket>> {
            Ok(self.tickets.borrow().iter().filter(|t| t.convoy.as_ref().is_some_and(|c| c.0 == parent.0)).cloned().collect())
        }
        fn read(&self, key: &TicketKey) -> Result<domain::TicketDetail> {
            let mut ticket = self.get(key)?;
            ticket.labels = self.labels.borrow().iter().filter(|(k, _)| k == key).map(|(_, l)| l.clone()).collect();
            Ok(domain::TicketDetail { ticket, kind: "task".into(), description: String::new(), related: vec![], held: None, location: String::new() })
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
                labels: vec![],
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
        fn unclaim(&self, key: &TicketKey) -> Result<()> {
            if let Some(t) = self.tickets.borrow_mut().iter_mut().find(|t| &t.key == key) {
                t.status = TicketStatus::Todo;
                t.assignee = None;
            }
            Ok(())
        }
        fn link(&self, _: &TicketKey, _: &TicketKey, _: domain::TicketLink, _: bool) -> Result<()> {
            Ok(())
        }
        fn comment(&self, key: &TicketKey, body: &str) -> Result<()> {
            self.comments.borrow_mut().push((key.clone(), body.into()));
            Ok(())
        }
    }

    #[test]
    fn re_opening_a_convoy_keeps_the_title_plan_or_goal_it_leaves_out() {
        let db = Ledger::in_memory().unwrap();
        let c9 = ConvoyKey("PROJ-9".into());
        let mut proof = tt("PROJ-1", TicketStatus::Todo);
        proof.convoy = Some(c9.clone());
        let tracker = FakeTracker::new(vec![proof]);
        let goal = TicketKey("PROJ-1".into());
        let title = |db: &Ledger| ConvoyRepo::active(db).unwrap()[0].title.clone();
        open_convoy(&db, &tracker, &c9, Some("Seed"), Some("ship the seed"), Some(&goal)).unwrap();
        open_convoy(&db, &tracker, &c9, None, Some("ship the seed, then the gate"), None).unwrap();
        assert_eq!(title(&db).as_deref(), Some("Seed"), "a new plan keeps the title");
        open_convoy(&db, &tracker, &c9, Some("Seed, gated"), None, None).unwrap();
        assert_eq!(ConvoyRepo::plan(&db, &c9).unwrap().as_deref(), Some("ship the seed, then the gate"), "a new title keeps the plan");
        assert_eq!(title(&db).as_deref(), Some("Seed, gated"));
        assert_eq!(ConvoyRepo::goal(&db, &c9).unwrap(), Some(goal), "and the goal");
        assert!(open_convoy(&db, &tracker, &c9, None, None, Some(&TicketKey("PROJ-404".into()))).is_err(), "a goal off the convoy");
    }

    #[test]
    fn a_captain_changes_only_its_convoys_tickets_and_each_amendment_is_recorded() {
        let db = Ledger::in_memory().unwrap();
        let c9 = ConvoyKey("PROJ-9".into());
        let mut mine = tt("PROJ-1", TicketStatus::Todo);
        mine.convoy = Some(c9.clone());
        let mut also = tt("PROJ-2", TicketStatus::Todo);
        also.convoy = Some(c9.clone());
        let tracker = FakeTracker::new(vec![mine, also, tt("PROJ-3", TicketStatus::Todo)]);
        sync::sync_board(&db, &tracker, "now").unwrap();
        let (one, two, three) = (TicketKey("PROJ-1".into()), TicketKey("PROJ-2".into()), TicketKey("PROJ-3".into()));

        assert!(in_convoy(&db, &c9, &one).is_ok() && in_convoy(&db, &c9, &two).is_ok());
        let e = in_convoy(&db, &c9, &three).unwrap_err().to_string();
        assert!(e.contains("isn't in convoy PROJ-9"), "{e}");

        let amend = Amend { captain: c9.clone(), why: "AC2 points at the blocker that owns it; behaviour kept".into(), case: Some(CaseId(12)) };
        amend_ticket(&db, &tracker, &one, "## ACs\n- AC2: as PROJ-2 delivers it\n", &amend).unwrap();
        let (on, note) = tracker.comments.borrow()[0].clone();
        assert_eq!(on, one);
        assert!(note.starts_with("Amended by the captain of convoy PROJ-9, case #12: AC2 points at"), "{note}");
        assert!(note.contains("```diff") && note.contains("+- AC2: as PROJ-2 delivers it"), "the diff rides along: {note}");
        assert_eq!(db.events_of(EventKind::TicketAmended).unwrap().len(), 1);
        assert!(amend_ticket(&db, &tracker, &three, "x", &amend).is_err(), "outside its convoy: refused");
        assert_eq!(tracker.comments.borrow().len(), 1, "…before anything is written");
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
    fn a_blocking_link_reaches_the_ledger_at_once_a_relation_never_does() {
        let db = Ledger::in_memory().unwrap();
        let tracker = FakeTracker::new(vec![tt("PROJ-1", TicketStatus::Todo), tt("PROJ-2", TicketStatus::Todo)]);
        sync::sync_board(&db, &tracker, "now").unwrap();
        let (one, two) = (TicketKey("PROJ-1".into()), TicketKey("PROJ-2".into()));
        let blockers = || TicketRepo::get(&db, &one).unwrap().unwrap().blocked_by;

        link_ticket(&db, &tracker, &one, &two, domain::TicketLink::BlockedBy, true).unwrap();
        assert_eq!(blockers(), vec![two.clone()], "held now, not at the next sync");
        link_ticket(&db, &tracker, &one, &two, domain::TicketLink::BlockedBy, true).unwrap();
        assert_eq!(blockers(), vec![two.clone()], "idempotent");
        link_ticket(&db, &tracker, &one, &two, domain::TicketLink::BlockedBy, false).unwrap();
        assert!(blockers().is_empty(), "freed now");
        link_ticket(&db, &tracker, &one, &two, domain::TicketLink::Relates, true).unwrap();
        assert!(blockers().is_empty(), "a relation never gates the line");
        link_ticket(&db, &tracker, &TicketKey("PROJ-404".into()), &one, domain::TicketLink::BlockedBy, true)
            .expect("off the board: the tracker's edge only, synced if it joins");
    }

    #[test]
    fn a_convoy_with_an_open_proposal_bound_for_it_wont_close() {
        let db = Ledger::in_memory().unwrap();
        let c9 = ConvoyKey("PROJ-9".into());
        let mut t = tt("PROJ-1", TicketStatus::Done);
        t.convoy = Some(c9.clone());
        let tracker = FakeTracker::new(vec![t]);
        sync::sync_board(&db, &tracker, "now").unwrap();
        ConvoyRepo::open(&db, &c9, None, "t").unwrap();
        let draft = TicketDraft { title: "Fix X".into(), kind: "bug".into(), description: "…".into(), convoy: None, blocked_by: vec![], labels: vec![] };
        let (id, key) = propose(&db, &tracker, Some(TicketKey("PROJ-1".into())), Proposed::New(draft)).unwrap();

        let err = close_convoy(&db, &c9).unwrap_err().to_string();
        assert!(err.contains(&format!("#{} {}", id.0, key.0)), "names it: {err}");
        assert_eq!(ConvoyRepo::active(&db).unwrap().len(), 1, "still open");

        drop_proposal(&db, &tracker, &id).unwrap();
        close_convoy(&db, &c9).unwrap();
        assert!(ConvoyRepo::active(&db).unwrap().is_empty(), "settled: it closes");
    }

    #[test]
    fn a_builder_cant_conclude_while_a_blocker_is_open() {
        let db = Ledger::in_memory().unwrap();
        let tracker = FakeTracker::new(vec![
            tt("PROJ-2", TicketStatus::InQa),
            TrackerTicket { blocked_by: vec![TicketKey("PROJ-2".into())], ..tt("PROJ-1", TicketStatus::InProgress) },
        ]);
        sync::sync_board(&db, &tracker, "now").unwrap();
        let worker = Session {
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
            ship: None,
        };
        SessionRepo::start(&db, &worker).unwrap().expect("session start");
        let me = ControlTarget::Cog { cog: CrewKind::Worker, ticket: TicketKey("PROJ-1".into()) };
        let concluded = CogState::Worker(WorkerState::Concluded);
        let e = record_status(&db, &FakeForge::default(), &me, concluded, None).unwrap_err().to_string();
        assert!(e.contains("blocked by PROJ-2"), "{e}");
        record_status(&db, &FakeForge::default(), &me, CogState::Worker(WorkerState::Waiting), None).unwrap();
        tracker.transition(&TicketKey("PROJ-2".into()), TicketStatus::Done).unwrap();
        sync::sync_board(&db, &tracker, "now").unwrap();
        let ship = our_ship(&db, "PROJ-1", 7);
        db.set_reviewed(&ship, Some("sha")).unwrap();
        record_status(&db, &FakeForge::default(), &me, concluded, None).unwrap();
    }

    #[test]
    fn an_escalation_is_told_its_case_and_a_second_one_to_wait() {
        let db = Ledger::in_memory().unwrap();
        sync::sync_board(&db, &FakeTracker::new(vec![tt("PROJ-1", TicketStatus::InProgress)]), "now").unwrap();
        let worker = Session {
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
            ship: None,
        };
        SessionRepo::start(&db, &worker).unwrap().expect("session start");
        let me = ControlTarget::Cog { cog: CrewKind::Worker, ticket: TicketKey("PROJ-1".into()) };
        let escalated = CogState::Worker(WorkerState::Escalated);
        let forge = FakeForge::default();
        let first = record_status(&db, &forge, &me, escalated, Some("X?".into())).unwrap();
        assert!(first.starts_with("case #1 raised"), "{first}");
        let again = record_status(&db, &forge, &me, escalated, Some("restarted; X?".into())).unwrap();
        assert!(again.starts_with("case #1 still awaits its answer") && again.contains("Wait in your pane"), "{again}");
        let implementing = CogState::Worker(WorkerState::Implementing);
        assert_eq!(record_status(&db, &forge, &me, implementing, None).unwrap(), "ok");
    }

    /// Our ship on `key` at `iid`, its ticket dispatched (an order), so the fleet owns it.
    fn our_ship(db: &Ledger, key: &str, iid: i64) -> ShipId {
        db.dispatch(&Order {
            ticket: TicketKey(key.into()),
            auto_impl: true,
            fork_from: "origin/main".into(),
            ac_summary: "do it".into(),
            park_at: None,
            worktree_path: Some("/wt".into()),
            created_at: "t".into(),
        })
        .unwrap();
        ShipRepo::upsert(
            db,
            &Ship {
                id: ShipId(0),
                ticket: Some(TicketKey(key.into())),
                iid,
                url: "u".into(),
                target: "master".into(),
                source: String::new(),
                pilot: "me".into(),
                created_at: "t".into(),
                ended_at: None,
                swept: None,
                reviewed: None,
                rounds: 0,
            },
        )
        .unwrap()
    }

    #[test]
    fn each_change_in_health_is_recorded_once_for_the_dashboards_light() {
        let (dir, checkout, _) = crate::git::fixture::checkout("tick-health", &[]);
        let cfg = crate::health::tests::cfg(&dir, &checkout);
        let db = Ledger::in_memory().unwrap();
        let mut last = None;
        let checks = |db: &Ledger| db.events_of(EventKind::HealthChecked).unwrap();
        let ref_guard = |e: &Event| e.payload["checks"].as_array().unwrap().iter().find(|c| c["name"] == "ref-guard").unwrap()["status"].clone();
        tick_health(&db, &cfg, &mut last).unwrap();
        tick_health(&db, &cfg, &mut last).unwrap();
        assert_eq!(checks(&db).len(), 1, "unguarded, once");
        assert_eq!(checks(&db)[0].payload["ok"], false);
        assert_eq!(ref_guard(&checks(&db)[0]), "fail", "every check named in it");

        let link = std::path::Path::new(&checkout).join(".git/hooks/reference-transaction");
        std::os::unix::fs::symlink(crate::health::kit_hooks().join(crate::health::REF_GUARD), link).unwrap();
        tick_health(&db, &cfg, &mut last).unwrap();
        assert_eq!(checks(&db).len(), 2);
        assert_eq!(ref_guard(&checks(&db)[1]), "ok", "guarded: recorded as it changes");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn crossings_set_an_mr_beside_our_open_tickets_and_what_their_mrs_share_with_it() {
        let db = Ledger::in_memory().unwrap();
        let c9 = ConvoyKey("PROJ-9".into());
        let in_c9 = |key, status| TrackerTicket { convoy: Some(c9.clone()), ..tt(key, status) };
        let tracker = FakeTracker::new(vec![
            in_c9("PROJ-1", TicketStatus::InProgress), // in the convoy, with our MR
            in_c9("PROJ-2", TicketStatus::Todo),       // in the convoy, no MR yet
            in_c9("PROJ-3", TicketStatus::Done),       // delivered: not beside anything
            tt("PROJ-4", TicketStatus::InQa),          // no convoy, but our MR carries it
            tt("PROJ-5", TicketStatus::InProgress),    // neither: not ours to hold it against
        ]);
        sync::sync_board(&db, &tracker, "now").unwrap();
        ConvoyRepo::open(&db, &c9, None, "t").unwrap();
        our_ship(&db, "PROJ-1", 11);
        our_ship(&db, "PROJ-4", 12);
        let ally = Ship {
            id: ShipId(0),
            ticket: None,
            iid: 77,
            url: "u".into(),
            target: "master".into(),
            source: "ally/x".into(),
            pilot: "ally".into(),
            created_at: "t".into(),
            ended_at: None,
            swept: None,
            reviewed: None,
            rounds: 0,
        };
        ShipRepo::upsert(&db, &ally).unwrap();
        let p = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let forge = FakeForge {
            paths: vec![(77, p(&["crates/a/src/x.rs", "crates/b/src/z.rs"])), (11, p(&["crates/a/src/x.rs", "crates/b/src/y.rs"]))],
            ..Default::default()
        };

        let c = crate::crossings::crossings(&db, &tracker, &forge, 77, false).unwrap();
        assert_eq!(c.paths, p(&["crates/a/src/x.rs", "crates/b/src/z.rs"]));
        let keys: Vec<&str> = c.tickets.iter().map(|t| t.key.0.as_str()).collect();
        assert_eq!(keys, ["PROJ-1", "PROJ-2", "PROJ-4"]);
        let one = &c.tickets[0];
        assert_eq!((one.iid, one.shared_paths.clone(), one.shared_folders.clone()), (Some(11), p(&["crates/a/src/x.rs"]), p(&["crates/b/src"])));
        assert!(c.tickets[1].iid.is_none() && c.tickets[1].shared_paths.is_empty(), "no MR: nothing shared, only its text");
        assert!(c.tickets.iter().all(|t| t.text.is_some()), "each with its text");
        assert!(crate::crossings::crossings(&db, &tracker, &forge, 77, true).unwrap().tickets.iter().all(|t| t.text.is_none()), "brief");
    }

    #[test]
    fn a_worker_concludes_only_once_a_lookout_reviewed_its_head_clean() {
        let db = Ledger::in_memory().unwrap();
        sync::sync_board(&db, &FakeTracker::new(vec![tt("PROJ-1", TicketStatus::InProgress)]), "now").unwrap();
        let key = TicketKey("PROJ-1".into());
        let worker = Session {
            id: SessionId(0),
            kind: CrewKind::Worker,
            ticket: Some(key.clone()),
            convoy: None,
            worktree_path: "/wt".into(),
            tmux_session: "worker-PROJ-1".into(),
            harness_session_id: None,
            created_at: "t".into(),
            ended_at: None,
            anchored_at: None,
            ship: None,
        };
        SessionRepo::start(&db, &worker).unwrap().expect("session start");
        let me = ControlTarget::Cog { cog: CrewKind::Worker, ticket: key.clone() };
        let (review, concluded) = (CogState::Worker(WorkerState::Review), CogState::Worker(WorkerState::Concluded));
        let forge = FakeForge::default();

        let e = record_status(&db, &forge, &me, review, None).unwrap_err().to_string();
        assert!(e.contains("no open MR"), "review needs the draft: {e}");
        let ship = our_ship(&db, "PROJ-1", 7);
        record_status(&db, &forge, &me, review, None).unwrap();
        let e = record_status(&db, &forge, &me, concluded, None).unwrap_err().to_string();
        assert!(e.contains("isn't reviewed"), "{e}");

        // A lookout is on it: reviewed at launch, but not yet finished.
        db.set_reviewed(&ship, Some("sha")).unwrap();
        let lookout = Session {
            kind: CrewKind::Lookout,
            ticket: None,
            tmux_session: "lookout-PROJ-1".into(),
            ship: Some(ship.clone()),
            ..worker.clone()
        };
        let lookout = SessionRepo::start(&db, &lookout).unwrap().expect("session start");
        assert!(record_status(&db, &forge, &me, concluded, None).is_err(), "the review is under way");
        SessionRepo::end(&db, &lookout, "t").unwrap();

        let threads = FakeForge { threads: 2, ..Default::default() };
        let e = record_status(&db, &threads, &me, concluded, None).unwrap_err().to_string();
        assert!(e.contains("2 open review threads"), "{e}");
        record_status(&db, &forge, &me, concluded, None).unwrap();
    }

    #[test]
    fn a_lookout_never_approves_our_own_mr() {
        let db = Ledger::in_memory().unwrap();
        sync::sync_board(&db, &FakeTracker::new(vec![tt("PROJ-1", TicketStatus::InProgress)]), "now").unwrap();
        our_ship(&db, "PROJ-1", 7);
        let post = |iid, approve| wire::ReviewPostCmd { iid, body: "clean".into(), approve, thread: false };
        let e = post_review(&db, &FakeForge::default(), &post(7, true)).unwrap_err().to_string();
        assert!(e.contains("never approves our own"), "{e}");
        post_review(&db, &FakeForge::default(), &post(7, false)).unwrap();
    }

    #[test]
    fn an_inspection_judges_the_cogs_current_escalation() {
        let db = Ledger::in_memory().unwrap();
        sync::sync_board(&db, &FakeTracker::new(vec![tt("PROJ-1", TicketStatus::InProgress)]), "now").unwrap();
        let key = TicketKey("PROJ-1".into());
        let worker = Session {
            id: SessionId(0),
            kind: CrewKind::Worker,
            ticket: Some(key.clone()),
            convoy: None,
            worktree_path: "/wt".into(),
            tmux_session: "worker-PROJ-1".into(),
            harness_session_id: None,
            created_at: "t".into(),
            ended_at: None,
            anchored_at: None,
            ship: None,
        };
        let id = SessionRepo::start(&db, &worker).unwrap().expect("session start");
        let report = |state, at: &str| Report { session: id.clone(), state, last_action: Some("X?".into()), reported_at: at.into() };
        ReportRepo::record(&db, &report(CogState::Worker(WorkerState::Implementing), "t1")).unwrap();
        assert!(inspect(&db, &key, Verdict::Confirm, "x".into()).is_err(), "nothing escalated to judge");
        ReportRepo::record(&db, &report(CogState::Worker(WorkerState::Escalated), "t2")).unwrap();
        let convoy = ConvoyKey("PROJ-100".into());
        add_to_convoy(&db, &FakeTracker::new(vec![tt("PROJ-1", TicketStatus::InProgress)]), &convoy, &key).unwrap();
        let flagged = |db: &Ledger| build_convoy_view(db, &convoy, true).unwrap().tickets[0].inspect;
        assert!(flagged(&db), "escalated, unjudged: the captain inspects it");
        inspect(&db, &key, Verdict::Confirm, "**Verdict:** CONFIRM".into()).unwrap();
        let got = db.inspection(&id).unwrap().unwrap();
        assert_eq!((got.reported_at.as_str(), got.verdict), ("t2", Verdict::Confirm), "judged against that report");
        assert!(!flagged(&db), "judged");
        let reworded = Report { last_action: Some("relaunched; still X?".into()), ..report(CogState::Worker(WorkerState::Escalated), "t3") };
        ReportRepo::record(&db, &reworded).unwrap();
        assert!(!flagged(&db), "awaiting its ruling: a re-report joins its judged case, whatever its words");
        inspect(&db, &key, Verdict::Refute, "**Verdict:** REFUTE".into()).unwrap();
        ReportRepo::record(&db, &report(CogState::Worker(WorkerState::Escalated), "t4")).unwrap();
        assert!(flagged(&db), "answered: another escalation is a new case, unjudged");
    }

    #[test]
    fn a_hand_is_done_only_with_no_mr_to_land() {
        let db = Ledger::in_memory().unwrap();
        sync::sync_board(&db, &FakeTracker::new(vec![tt("PROJ-1", TicketStatus::InProgress)]), "now").unwrap();
        let key = TicketKey("PROJ-1".into());
        let hand = Session {
            id: SessionId(0),
            kind: CrewKind::Hand,
            ticket: Some(key.clone()),
            convoy: None,
            worktree_path: "/wt".into(),
            tmux_session: "hand-PROJ-1".into(),
            harness_session_id: None,
            created_at: "t".into(),
            ended_at: None,
            anchored_at: None,
            ship: None,
        };
        SessionRepo::start(&db, &hand).unwrap().expect("session start");
        let ship = Ship {
            id: ShipId(0),
            ticket: Some(key.clone()),
            iid: 7,
            url: "u".into(),
            target: "master".into(),
            source: "chore/PROJ-1".into(),
            pilot: "me".into(),
            created_at: "t".into(),
            ended_at: None,
            swept: None,
            reviewed: None,
            rounds: 0,
        };
        ShipRepo::upsert(&db, &ship).unwrap();
        let me = ControlTarget::Cog { cog: CrewKind::Hand, ticket: key.clone() };
        let done = CogState::Hand(HandState::Done);
        let e = record_status(&db, &FakeForge::default(), &me, done, None).unwrap_err().to_string();
        assert!(e.contains("open MR"), "{e}");
        let id = db.armada().unwrap()[0].id.clone();
        ShipRepo::retire(&db, &id, "t").unwrap();
        record_status(&db, &FakeForge::default(), &me, done, None).unwrap();
    }

    #[test]
    fn a_ticketless_cog_reports_by_its_convoy_or_as_the_lookout() {
        let db = Ledger::in_memory().unwrap();
        ConvoyRepo::open(&db, &ConvoyKey("PROJ-100".into()), None, "t").unwrap();
        let captain = start_ticketless(&db, CrewKind::Captain, Some("PROJ-100"), "captain-PROJ-100");
        let lookout = start_ticketless(&db, CrewKind::Lookout, None, "lookout");
        let cap = |c: &str| ControlTarget::Captain { convoy: ConvoyKey(c.into()) };
        let done = CogState::Captain(CaptainState::Done);
        assert!(record_status(&db, &FakeForge::default(), &cap("PROJ-200"), done, None).is_err(), "no captain on PROJ-200");
        record_status(&db, &FakeForge::default(), &cap("PROJ-100"), done, None).unwrap();
        assert_eq!(ReportRepo::latest(&db, &captain).unwrap().map(|r| r.state), Some(done));
        let reviewed = CogState::Lookout(LookoutState::Reviewed);
        record_status(&db, &FakeForge::default(), &ControlTarget::Lookout { iid: None }, reviewed, None).unwrap();
        assert_eq!(ReportRepo::latest(&db, &lookout).unwrap().map(|r| r.state), Some(reviewed));
    }

    #[test]
    fn a_lookout_reports_on_its_own_ship_among_several() {
        let db = Ledger::in_memory().unwrap();
        let on = |iid: i64| {
            let ship = Ship {
                id: ShipId(0),
                ticket: None,
                iid,
                url: "u".into(),
                target: "master".into(),
                source: String::new(),
                pilot: "ally".into(),
                created_at: "t".into(),
                ended_at: None,
                swept: None,
                reviewed: None,
                rounds: 0,
            };
            let ship = ShipRepo::upsert(&db, &ship).unwrap();
            let id = start_ticketless(&db, CrewKind::Lookout, None, &format!("lookout-{iid}"));
            let lookout = Session { ship: Some(ship), ..SessionRepo::get(&db, &id).unwrap().unwrap() };
            SessionRepo::end(&db, &id, "t").unwrap(); // re-start it on its ship
            SessionRepo::start(&db, &lookout).unwrap().expect("one lookout per ship")
        };
        let (a, b) = (on(77), on(78));
        let reviewed = CogState::Lookout(LookoutState::Reviewed);
        record_status(&db, &FakeForge::default(), &ControlTarget::Lookout { iid: Some(78) }, reviewed, None).unwrap();
        assert_eq!(ReportRepo::latest(&db, &b).unwrap().map(|r| r.state), Some(reviewed));
        assert!(ReportRepo::latest(&db, &a).unwrap().is_none(), "!77's lookout didn't report");
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
            labels: vec![],
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
    fn reset_rounds_takes_the_tickets_open_mr_back_to_zero_and_refuses_one_with_none() {
        let db = Ledger::in_memory().unwrap();
        let tracker = FakeTracker::new(vec![tt("PROJ-1", TicketStatus::InQa)]);
        sync::sync_board(&db, &tracker, "now").unwrap();
        let key = TicketKey("PROJ-1".into());
        let e = reset_rounds(&db, &key).unwrap_err().to_string();
        assert!(e.contains("no open MR"), "{e}");

        let ship = Ship {
            id: ShipId(0),
            ticket: Some(key.clone()),
            iid: 7,
            url: "u".into(),
            target: "master".into(),
            source: String::new(),
            pilot: "me".into(),
            created_at: "t".into(),
            ended_at: None,
            swept: None,
            reviewed: None,
            rounds: 0,
        };
        let id = ShipRepo::upsert(&db, &ship).unwrap();
        for _ in 0..10 {
            ShipRepo::count_round(&db, &id).unwrap();
        }
        reset_rounds(&db, &key).unwrap();
        assert_eq!(ShipRepo::armada(&db).unwrap()[0].rounds, 0, "ten more sweeps before the cap");
    }

    #[test]
    fn rework_sends_the_ticket_back_to_a_builder_in_its_worktree_once_its_blockers_are_done() {
        let db = Ledger::in_memory().unwrap();
        let blocked = TrackerTicket { blocked_by: vec![TicketKey("PROJ-2".into())], ..tt("PROJ-1", TicketStatus::InQa) };
        let tracker = FakeTracker::new(vec![blocked, tt("PROJ-2", TicketStatus::InProgress)]);
        sync::sync_board(&db, &tracker, "now").unwrap();
        let key = TicketKey("PROJ-1".into());
        let forge = FakeForge::default();
        let host = FakeHost::new();
        let e = rework(&db, &tracker, &forge, &host, &key, false).unwrap_err().to_string();
        assert!(e.contains("never dispatched"), "{e}");

        let at = "2026-01-01T00:00:00Z";
        let order = Order {
            ticket: key.clone(),
            auto_impl: true,
            fork_from: "origin/master".into(),
            ac_summary: "s".into(),
            park_at: None,
            worktree_path: Some("/wt".into()),
            created_at: at.into(),
        };
        OrderRepo::dispatch(&db, &order).unwrap();
        let ship = Ship {
            id: ShipId(0),
            ticket: Some(key.clone()),
            iid: 7,
            url: "u".into(),
            target: "master".into(),
            source: String::new(),
            pilot: "me".into(),
            created_at: at.into(),
            ended_at: None,
            swept: None,
            reviewed: None,
            rounds: 0,
        };
        ShipRepo::upsert(&db, &ship).unwrap();
        let sailor = Session {
            id: SessionId(0),
            kind: CrewKind::Sailor,
            ticket: Some(key.clone()),
            convoy: None,
            worktree_path: "/wt".into(),
            tmux_session: "sailor-PROJ-1".into(),
            harness_session_id: None,
            created_at: at.into(),
            ended_at: None,
            anchored_at: None,
            ship: None,
        };
        SessionRepo::start(&db, &sailor).unwrap().expect("session start");

        rework(&db, &tracker, &forge, &host, &key, false).unwrap();
        assert_eq!(TicketRepo::get(&db, &key).unwrap().unwrap().status, Some(TicketStatus::Todo), "back in the line");
        assert!(live_cogs_on(&db, &key).unwrap().is_empty(), "its sailor is sunk");
        assert_eq!(*host.killed.borrow(), vec!["sailor-PROJ-1".to_string()]);
        assert_eq!((forge.disarmed.borrow().clone(), forge.drafted.borrow().clone()), (vec![7], vec![(7, true)]));
        assert!(OrderRepo::get(&db, &key).unwrap().unwrap().created_at.as_str() > at, "the order is re-dated");
        // The worker waits for its blocker, and no sailor launches on a ticket back in To Do.
        assert!(reconcile::plan(&db, &[], true, &LEVERS).unwrap().is_empty());
        tracker.transition(&TicketKey("PROJ-2".into()), TicketStatus::Done).unwrap();
        sync::sync_board(&db, &tracker, "now").unwrap();
        assert_eq!(
            reconcile::plan(&db, &[], true, &LEVERS).unwrap(),
            vec![reconcile::Action::Spawn { ticket: "PROJ-1".into(), kind: "worker".into() }]
        );

        // --hand: no move, a manual order, a hand at once — blockers or not.
        tracker.transition(&key, TicketStatus::InQa).unwrap();
        tracker.transition(&TicketKey("PROJ-2".into()), TicketStatus::InProgress).unwrap();
        sync::sync_board(&db, &tracker, "now").unwrap();
        rework(&db, &tracker, &forge, &host, &key, true).unwrap();
        assert_eq!(TicketRepo::get(&db, &key).unwrap().unwrap().status, Some(TicketStatus::InQa));
        assert!(!OrderRepo::get(&db, &key).unwrap().unwrap().auto_impl);
        assert_eq!(
            reconcile::plan(&db, &[], true, &LEVERS).unwrap(),
            vec![reconcile::Action::Spawn { ticket: "PROJ-1".into(), kind: "hand".into() }]
        );
    }

    #[test]
    fn a_convoy_anchor_holds_its_captain_and_open_tickets_and_its_weigh_releases_just_those() {
        let db = Ledger::in_memory().unwrap();
        let c9 = ConvoyKey("PROJ-9".into());
        let in_c9 = |key: &str, status| TrackerTicket { convoy: Some(c9.clone()), ..tt(key, status) };
        let tracker = FakeTracker::new(vec![
            in_c9("PROJ-1", TicketStatus::InQa),
            in_c9("PROJ-2", TicketStatus::Todo),
            in_c9("PROJ-3", TicketStatus::Done),
            tt("PROJ-4", TicketStatus::InQa),
        ]);
        sync::sync_board(&db, &tracker, "now").unwrap();
        ConvoyRepo::open(&db, &c9, None, "t").unwrap();
        let (one, two, three, four) =
            (TicketKey("PROJ-1".into()), TicketKey("PROJ-2".into()), TicketKey("PROJ-3".into()), TicketKey("PROJ-4".into()));
        TicketRepo::set_held(&db, &two, true, Some("not Ready: AC2 unclear"), "t").unwrap(); // held already, its own reason
        let ship = Ship {
            id: ShipId(0),
            ticket: Some(one.clone()),
            iid: 7,
            url: "u".into(),
            target: "master".into(),
            source: String::new(),
            pilot: "me".into(),
            created_at: "t".into(),
            ended_at: None,
            swept: None,
            reviewed: None,
            rounds: 0,
        };
        ShipRepo::upsert(&db, &ship).unwrap();
        let sailor = Session {
            id: SessionId(0),
            kind: CrewKind::Sailor,
            ticket: Some(one.clone()),
            convoy: None,
            worktree_path: "/wt".into(),
            tmux_session: "sailor-PROJ-1".into(),
            harness_session_id: None,
            created_at: "t".into(),
            ended_at: None,
            anchored_at: None,
            ship: None,
        };
        let sid = SessionRepo::start(&db, &sailor).unwrap().unwrap();
        let (forge, host) = (FakeForge::default(), FakeHost::new());
        let held = |k: &TicketKey| TicketRepo::get(&db, k).unwrap().unwrap().held;
        let target = ControlTarget::Captain { convoy: c9.clone() };

        control(&db, &host, Verb::Anchor, &target).unwrap();
        hold_convoy(&db, &forge, &host, &target, true).unwrap();
        assert!(ConvoyRepo::active(&db).unwrap()[0].anchored_at.is_some(), "its captain is held: nothing new dispatched");
        assert!(held(&one) && db.held_reason(&one).unwrap().as_deref() == Some("convoy PROJ-9 anchored"));
        assert!(SessionRepo::get(&db, &sid).unwrap().unwrap().anchored_at.is_some(), "its sailor interrupted, not killed");
        assert!(SessionRepo::get(&db, &sid).unwrap().unwrap().ended_at.is_none());
        assert_eq!((forge.disarmed.borrow().clone(), forge.drafted.borrow().clone()), (vec![7], vec![(7, true)]), "its MR drafted");
        assert_eq!(db.held_reason(&two).unwrap().as_deref(), Some("not Ready: AC2 unclear"), "its own hold kept");
        assert!(!held(&three) && !held(&four), "a Done ticket, and one outside the convoy, untouched");

        control(&db, &host, Verb::Weigh, &target).unwrap();
        hold_convoy(&db, &forge, &host, &target, false).unwrap();
        assert!(ConvoyRepo::active(&db).unwrap()[0].anchored_at.is_none());
        assert!(!held(&one) && SessionRepo::get(&db, &sid).unwrap().unwrap().anchored_at.is_none(), "released, its sailor resumed");
        assert_eq!(forge.drafted.borrow().last(), Some(&(7, false)), "its MR ready again (In QA)");
        assert!(held(&two), "a hold of its own outlives the convoy's weigh");
    }

    #[test]
    fn anchoring_a_ticket_holds_it_stops_its_cog_and_drafts_its_mr_and_weigh_releases_all() {
        // An armed MR merges on the forge's own say — so anchoring the ticket disarms it, clears
        // `swept` so the first pass after weigh re-sweeps (a fresh sailor re-arms), and drafts it;
        // its live cog is anchored too.
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
            rounds: 0,
        };
        let sid = ShipRepo::upsert(&db, &ship).unwrap();
        db.set_swept(&sid, Some("fp")).unwrap();
        let forge = FakeForge::default();
        let key = TicketKey("PROJ-1".into());

        let mut sailor = Session {
            id: SessionId(0),
            kind: CrewKind::Sailor,
            ticket: Some(key.clone()),
            convoy: None,
            worktree_path: "/wt".into(),
            tmux_session: "sailor-PROJ-1".into(),
            harness_session_id: None,
            created_at: "t".into(),
            ended_at: None,
            anchored_at: None,
            ship: None,
        };
        sailor.id = SessionRepo::start(&db, &sailor).unwrap().expect("session start");
        let admiral = Session { kind: CrewKind::Admiral, tmux_session: "admiral-PROJ-1".into(), ..sailor.clone() };
        let admiral = SessionRepo::start(&db, &admiral).unwrap().expect("session start");
        let (host, anchored) = (FakeHost::new(), |db: &Ledger| SessionRepo::get(db, &sailor.id).unwrap().unwrap().anchored_at.is_some());

        anchor_ticket(&db, &forge, &host, &key, true, Some("late dependency")).unwrap();
        assert_eq!(*forge.disarmed.borrow(), vec![7], "anchoring disarmed !7");
        assert_eq!(db.get_by_iid(7).unwrap().unwrap().swept, None, "swept cleared → re-sweep on weigh");
        assert!(TicketRepo::get(&db, &key).unwrap().unwrap().held);
        assert!(anchored(&db), "its sailor is anchored");
        assert!(SessionRepo::get(&db, &admiral).unwrap().unwrap().anchored_at.is_none(), "its admiral is the operator's seat");

        anchor_ticket(&db, &forge, &host, &key, false, None).unwrap();
        assert_eq!(forge.disarmed.borrow().len(), 1, "weigh doesn't disarm");
        assert_eq!(*forge.drafted.borrow(), vec![(7, true), (7, false)], "anchor drafts, weigh readies");
        assert!(!TicketRepo::get(&db, &key).unwrap().unwrap().held);
        assert!(!anchored(&db), "its sailor resumes");
        assert_eq!(host.sent.borrow().len(), 2, "the stop, then the resume, reached its sailor's pane alone");

        // In the worker's phase the MR was a draft before the anchor: weigh leaves it one.
        let worker_phase = FakeTracker::new(vec![tt("PROJ-1", TicketStatus::InProgress)]);
        sync::sync_board(&db, &worker_phase, "now").unwrap();
        anchor_ticket(&db, &forge, &host, &key, true, None).unwrap();
        anchor_ticket(&db, &forge, &host, &key, false, None).unwrap();
        assert_eq!(forge.drafted.borrow().last(), Some(&(7, true)), "a worker's MR stays a draft");

        // A forge without drafts: the hold + disarm stand, and the caller hears what failed.
        let no_drafts = FakeForge { no_drafts: true, ..Default::default() };
        let e = format!("{:#}", anchor_ticket(&db, &no_drafts, &host, &key, true, None).unwrap_err());
        assert!(e.contains("anchored: disarmed, but couldn't mark !7 draft"), "{e}");
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
        drop(opened);

        // Its MR already open (a rework relaunching): rewritten in place, no second MR.
        let ship = Ship {
            id: ShipId(0),
            ticket: Some(TicketKey("PROJ-2".into())),
            iid: 9,
            url: "https://forge/mr/9".into(),
            target: "main".into(),
            source: "chore/PROJ-2-child".into(),
            pilot: "me".into(),
            created_at: "t".into(),
            ended_at: None,
            swept: None,
            reviewed: None,
            rounds: 0,
        };
        ShipRepo::upsert(&db, &ship).unwrap();
        assert_eq!(launch(&db, &forge, req(Some(TicketKey("PROJ-2".into())), None)).unwrap(), "https://forge/mr/9");
        assert_eq!(*forge.noted.borrow(), vec![(9, "PROJ-2: child".to_string())]);
        assert_eq!(forge.opened.borrow().len(), 3, "no second MR");
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
            rounds: 0,
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
        assert!(arm(&db, &forge, &key, true).unwrap_err().to_string().contains("is anchored"));

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
            labels: vec![],
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

    /// Records kills + sends (where, and what); the rest are harmless defaults.
    struct FakeHost {
        killed: RefCell<Vec<String>>,
        sent: RefCell<Vec<String>>,
        said: RefCell<Vec<String>>,
    }
    impl FakeHost {
        fn new() -> Self {
            Self { killed: RefCell::new(vec![]), sent: RefCell::new(vec![]), said: RefCell::new(vec![]) }
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
        fn send(&self, name: &str, text: &str) -> Result<()> {
            self.sent.borrow_mut().push(name.into());
            self.said.borrow_mut().push(text.into());
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
                ship: None,
            },
        )
        .unwrap()
        .unwrap();
        (db, id)
    }

    #[test]
    fn a_ruling_is_recorded_then_sent_to_its_escalated_cog() {
        let (db, id) = db_with_live_worker();
        let host = FakeHost::new();
        let key = TicketKey("PROJ-1".into());
        let state = CogState::Worker(WorkerState::Escalated);
        ReportRepo::record(&db, &Report { session: id.clone(), state, last_action: Some("which auth?".into()), reported_at: "t1".into() }).unwrap();
        let case = db.open_case(&id).unwrap().expect("raised");
        assert!(rule(&db, &host, Some(case.id.clone()), None, "Which auth?".into(), "Rev 4's.".into()).is_err(), "not confirmed yet");
        assert!(host.sent.borrow().is_empty(), "nothing reaches the cog unrecorded");

        inspect(&db, &key, Verdict::Confirm, "**Verdict:** CONFIRM — the operator's".into()).unwrap();
        let said = rule(&db, &host, Some(case.id.clone()), None, "Which auth does an admin route take?".into(), "Rev 4's.\nAll of them.".into()).unwrap();
        assert!(said.contains("sent to worker-PROJ-1"), "{said}");
        assert_eq!(*host.sent.borrow(), vec!["worker-PROJ-1".to_string()]);
        assert!(!host.said.borrow()[0].contains("prepared"), "no prepared part: {}", host.said.borrow()[0]);
        assert_eq!(db.case(&case.id).unwrap().unwrap().ruling.unwrap().answer, "Rev 4's.\nAll of them.", "recorded in full");

        // A confirm with a prepared part: it goes to the cog with the ruling, at once.
        let state = CogState::Worker(WorkerState::Implementing);
        ReportRepo::record(&db, &Report { session: id.clone(), state, last_action: None, reported_at: "t2".into() }).unwrap();
        let state = CogState::Worker(WorkerState::Escalated);
        ReportRepo::record(&db, &Report { session: id.clone(), state, last_action: Some("which key?".into()), reported_at: "t3".into() }).unwrap();
        let next = db.open_case(&id).unwrap().unwrap();
        inspect(&db, &key, Verdict::Confirm, "**Verdict:** CONFIRM — x\n- **Prepared:** answer threads 4, 7".into()).unwrap();
        rule(&db, &host, Some(next.id.clone()), None, "Which key?".into(), "The seat's.".into()).unwrap();
        let said = host.said.borrow()[1].clone();
        assert!(said.contains("The seat's.") && said.contains("the verdict's prepared steps"), "{said}");

        let alone = rule(&db, &host, None, Some(key), "Q?".into(), "A.".into()).unwrap();
        assert!(alone.contains("ruled on PROJ-1"), "{alone}");
        assert_eq!(host.sent.borrow().len(), 2, "a ruling with no escalation has no cog to send to");
        assert!(rule(&db, &host, None, None, "Q?".into(), " ".into()).is_err(), "a ruling has an answer");
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
            rounds: 0,
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
    fn recall_closes_the_mr_forgets_the_work_and_returns_the_ticket_anchored() {
        let (db, id) = db_with_live_worker();
        let key = TicketKey("PROJ-1".into());
        let ship = Ship {
            id: ShipId(0),
            ticket: Some(key.clone()),
            iid: 7,
            url: "u".into(),
            target: "master".into(),
            source: String::new(),
            pilot: "me".into(),
            created_at: "t".into(),
            ended_at: None,
            swept: None,
            reviewed: None,
            rounds: 0,
        };
        ShipRepo::upsert(&db, &ship).unwrap();
        let mut claimed = tt("PROJ-1", TicketStatus::InProgress);
        claimed.assignee = Some("me".into());
        let tracker = FakeTracker::new(vec![claimed]);
        let host = FakeHost::new();

        let merged = FakeForge { state: Some(ShipState::Merged), ..Default::default() };
        assert!(recall(&db, &merged, &tracker, &host, "/p", &key, "wrong approach", false).is_err(), "a merged MR is refused");
        assert!(SessionRepo::get(&db, &id).unwrap().unwrap().ended_at.is_none(), "untouched");

        let forge = FakeForge { state: Some(ShipState::Open), ..Default::default() };
        let said = recall(&db, &forge, &tracker, &host, "/p", &key, "wrong approach", false).unwrap();
        assert_eq!((forge.disarmed.borrow().clone(), forge.closed.borrow().clone()), (vec![7], vec![7]), "disarmed, then closed");
        assert_eq!(*forge.noted.borrow(), vec![(7, "Recalled to the backlog: wrong approach".to_string())]);
        let t = TicketRepo::get(&db, &key).unwrap().unwrap();
        assert_eq!((t.status, t.assignee, t.held), (Some(TicketStatus::Todo), None, true), "back to To Do, unassigned, anchored");
        assert!(db.held_reason(&key).unwrap().is_some_and(|r| r.contains("wrong approach")));
        assert!(SessionRepo::get(&db, &id).unwrap().unwrap().ended_at.is_some(), "its cog ended");
        assert!(db.armada().unwrap().is_empty(), "the ship ended");
        assert!(said.contains("closed !7") && said.contains("To Do") && said.contains("anchored"), "said: {said}");
        assert!(db.since(Seq(0)).unwrap().iter().any(|e| e.kind == EventKind::TicketRecalled));
    }

    #[test]
    fn recall_with_weigh_returns_the_ticket_unanchored_even_one_held_before() {
        let (db, id) = db_with_live_worker();
        let key = TicketKey("PROJ-1".into());
        TicketRepo::set_held(&db, &key, true, Some("waits on PROJ-2"), "t").unwrap();
        let tracker = FakeTracker::new(vec![tt("PROJ-1", TicketStatus::InProgress)]);

        let said = recall(&db, &FakeForge::default(), &tracker, &FakeHost::new(), "/p", &key, "dispatched too early", true).unwrap();
        let t = TicketRepo::get(&db, &key).unwrap().unwrap();
        assert_eq!((t.status, t.held), (Some(TicketStatus::Todo), false), "back to To Do, weighed");
        assert!(SessionRepo::get(&db, &id).unwrap().unwrap().ended_at.is_some(), "its cog ended");
        assert!(said.contains("weighed") && !said.contains("anchored"), "said: {said}");
        let ev = db.since(Seq(0)).unwrap();
        assert!(ev.iter().any(|e| e.kind == EventKind::TicketRecalled && e.payload["weighed"] == true));
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
                ship: None,
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
    fn lookout_anchor_toggles_the_review_loop_gate() {
        let db = Ledger::in_memory().unwrap();
        let host = FakeHost::new();
        control(&db, &host, Verb::Anchor, &ControlTarget::Lookout { iid: None }).unwrap();
        assert!(LookoutRepo::anchored(&db).unwrap(), "the lookout row holds the hold");
        control(&db, &host, Verb::Weigh, &ControlTarget::Lookout { iid: None }).unwrap();
        assert!(!LookoutRepo::anchored(&db).unwrap(), "weigh lifts it");
    }

    #[test]
    fn lookout_sink_anchors_and_scuttles_the_live_lookout() {
        let db = Ledger::in_memory().unwrap();
        let id = start_ticketless(&db, CrewKind::Lookout, None, "lookout");
        let host = FakeHost::new();
        control(&db, &host, Verb::Sink, &ControlTarget::Lookout { iid: None }).unwrap();
        assert!(LookoutRepo::anchored(&db).unwrap(), "sinking every lookout holds the loop");
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
                rounds: 0,
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
            total_threads: 0,
            approvals: Approvals { current: 0, required: 0 },
            armed: false,
            on_merge_train: false,
            blocked_by: vec![],
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
            labels: vec![],
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
            rounds: 0,
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
    fn the_convoy_view_marks_the_goal_and_what_it_waits_on_as_critical() {
        use domain::ports::TicketRepo;
        let db = Ledger::in_memory().unwrap();
        let t = |key: &str, blocked_by: &[&str]| Ticket {
            key: TicketKey(key.into()),
            convoy: Some(ConvoyKey("PROJ-100".into())),
            held: false,
            created_at: "t".into(),
            blocked_by: blocked_by.iter().map(|k| TicketKey((*k).into())).collect(),
            title: None,
            assignee: None,
            status: Some(TicketStatus::Todo),
            synced_at: None,
        };
        // PROJ-1 → PROJ-2 (the goal); PROJ-3 is a parallel stream.
        for ticket in [t("PROJ-1", &[]), t("PROJ-2", &["PROJ-1"]), t("PROJ-3", &[])] {
            TicketRepo::upsert(&db, &ticket).unwrap();
            TicketRepo::set_blocked_by(&db, &ticket.key, &ticket.blocked_by).unwrap();
        }
        let convoy = ConvoyKey("PROJ-100".into());
        let critical = |db: &Ledger| -> Vec<String> {
            build_convoy_view(db, &convoy, true).unwrap().tickets.into_iter().filter(|t| t.critical).map(|t| t.key.0).collect()
        };
        db.open(&convoy, None, "t").unwrap();
        assert!(critical(&db).is_empty(), "no goal, no path");
        db.set_goal(&convoy, &TicketKey("PROJ-2".into()), "t").unwrap();
        let mut path = critical(&db);
        path.sort();
        assert_eq!(path, ["PROJ-1", "PROJ-2"]);
        assert_eq!(build_convoy_view(&db, &convoy, true).unwrap().goal, Some(TicketKey("PROJ-2".into())));
    }

    #[test]
    fn prune_runs_the_projects_command_in_its_checkout() {
        let dir = std::env::temp_dir().join(format!("fleetd-prune-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        prune("touch pruned", &dir.to_string_lossy()).join().unwrap();
        assert!(dir.join("pruned").exists(), "run in the checkout");
        prune("exit 3", &dir.to_string_lossy()).join().unwrap(); // a failure is logged, never fatal
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_ticket_added_for_a_hand_shows_so_in_its_convoy_view() {
        let db = Ledger::in_memory().unwrap();
        let tracker = FakeTracker::new(vec![tt("PROJ-1", TicketStatus::Todo)]);
        let (convoy, key) = (ConvoyKey("PROJ-100".into()), TicketKey("PROJ-1".into()));
        let hand = |db: &Ledger| build_convoy_view(db, &convoy, true).unwrap().tickets.iter().find(|t| t.key == key).map(|t| t.hand);
        add_to_convoy(&db, &tracker, &convoy, &key).unwrap();
        TicketRepo::set_hand(&db, &key, true, "now").unwrap();
        assert_eq!(hand(&db), Some(true), "for a hand");
        TicketRepo::set_hand(&db, &key, false, "now").unwrap();
        assert_eq!(hand(&db), Some(false), "added again without --hand: for a worker");
    }

    #[test]
    fn a_product_owner_ticket_is_dispatched_to_a_hand_whatever_the_producer_asked() {
        let db = Ledger::in_memory().unwrap();
        let po = TrackerTicket { labels: vec![TicketLabel::ProductOwner.as_str().into()], ..tt("PROJ-1", TicketStatus::Todo) };
        sync::sync_board(&db, &FakeTracker::new(vec![po, tt("PROJ-2", TicketStatus::Todo)]), "now").unwrap();
        let order = |key: &str| Order {
            ticket: TicketKey(key.into()),
            auto_impl: true,
            fork_from: "origin/master".into(),
            ac_summary: "s".into(),
            park_at: None,
            worktree_path: Some("/wt".into()),
            created_at: "now".into(),
        };
        let auto = |key: &str| OrderRepo::get(&db, &TicketKey(key.into())).unwrap().unwrap().auto_impl;
        dispatch(&db, order("PROJ-1")).unwrap();
        dispatch(&db, order("PROJ-2")).unwrap();
        assert!(!auto("PROJ-1"), "labelled: a hand");
        assert!(auto("PROJ-2"), "unlabelled: a worker");
    }

    #[test]
    fn control_target_from_wire_branches_on_cog_and_validates_its_key() {
        let ct = |cog: &str, ticket: Option<&str>, convoy: Option<&str>| wire::CogTarget {
            cog: cog.into(),
            ticket: ticket.map(Into::into),
            convoy: convoy.map(Into::into),
            iid: None,
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
        // lookout → an MR, or none (every lookout).
        assert!(matches!(ControlTarget::from_wire(ct("lookout", None, None)), Ok(ControlTarget::Lookout { iid: None })));
        let one = wire::CogTarget { iid: Some(7), ..ct("lookout", None, None) };
        assert!(matches!(ControlTarget::from_wire(one), Ok(ControlTarget::Lookout { iid: Some(7) })));
        // unknown cog → a clear error, never a silent accept.
        assert!(ControlTarget::from_wire(ct("foreman", Some("PROJ-1"), None)).is_err());
    }
}
