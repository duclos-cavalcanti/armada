//! reconcile — the gap-closer (PLAN §reconcile), fleetd's ledger-driven mirror of
//! converge.sh. It closes the gap between intent (orders/board) and reality
//! (sessions/ships/Jira) via four moves: **spawn, handoff, reap, heal**.
//!
//! `plan` computes the actions (reads only). `tick(…, dry_run)` applies them through the
//! ports: `dry_run=true` records the plan as a `TickRan` event and executes nothing;
//! `dry_run=false` applies each action (Spawn is live; handoff/reap/heal land next) and
//! records what ran + what failed (a per-action failure is never fatal). The scheduler
//! still calls `tick_dry_run` until config wires the flag (PLAN §7 Gate D/E).
//!
//! Triggers blend the cog's self-REPORT (`fleetc status`) with DERIVED liveness:
//! - spawn:    a Ready, dispatched ticket (an `Order`) with no session.
//! - handoff:  a live builder (worker/hand) whose latest report is `concluded`, with
//!   no live sailor — the reported state is the signal, not `ended_at`.
//! - reap:     a merged ship (while a live session OR its open order remains) or a
//!   terminal ticket with a live session.
//! - heal:     a Dead WARM cog (worker/hand) that didn't deliberately stop — a crash → respawn.
//! - stand_down: a Dead COLD cog (sailor) — its one-shot watch is over → end the session so the
//!   sweep trigger can re-launch on the next change (a crash looks identical; it re-fires too).
//! - lookout:   one SINGLETON cog reviewing ALLIED ships (not ours). An allied open ship whose code
//!   changed since its last review (head_sha ≠ reviewed) → launch the lookout on it (fleetd picks
//!   the iid + marks it reviewed at dispatch); it reviews one and reports `reviewed`, reconcile
//!   stands it down, and the next tick takes the next ally.
//! - captain:   one cold captain per ACTIVE convoy — launched when the convoy has a pass of work
//!   (a ready-undispatched ticket, or all delivered → close), stood down when it reports `done`.

use crate::now_rfc3339;
use crate::Cog;
use anyhow::{anyhow, Result};
use domain::ports::{
    ConvoyRepo, EventLog, Forge, LookoutRepo, OrderRepo, ReportRepo, SessionHost, SessionRepo,
    ShipRepo, TicketRepo, Tracker,
};
use domain::{
    Classification, ConvoyKey, CrewKind, EntityKind, EventKind, Liveness, Order, PipelineStatus,
    Session, SessionId, Ship, ShipSnapshot, ShipState, Ticket, TicketKey, TicketStatus,
};
use ledger::Ledger;
use serde::Serialize;
use std::path::Path;

/// A planned reconcile action (the intent; the executor applies it later).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Action {
    Spawn { ticket: String, kind: String },    // launch the crew for a Ready dispatched ticket
    Handoff { ticket: String },                // builder done → Jira In QA + reap the builder
    Reap { ticket: String },                   // merged/terminal → end session + Jira Done
    Heal { ticket: String, kind: String },     // dead WARM session (worker/hand) → respawn in place
    StandDown { ticket: String, kind: String }, // dead COLD session (sailor) → end it; re-spawn on change
    LaunchCaptain { convoy: String },          // active convoy with a pass of work + no live captain → launch
    StandDownCaptain { convoy: String },       // captain finished its pass / convoy gone → end its session
    LaunchSailor { ticket: String },           // open ship changed since last sweep → launch a sailor
    LaunchLookout { iid: i64 },                // an allied ship changed since last review → launch the lookout on it
    StandDownLookout,                          // the singleton lookout finished its review → end its session
}

/// Compute the reconcile plan from ledger state + the live cogs (session + derived
/// liveness). Pure (reads only). `cogs` carry liveness for the heal/terminal checks; `stacks` is
/// the forge's capability (`Forge::stacks`) — without it, no STACKED spawn and no BLOCK.
pub fn plan(ledger: &Ledger, cogs: &[Cog], stacks: bool) -> Result<Vec<Action>> {
    let mut actions = Vec::new();

    // DISPATCH — iterate the order queue (intent). Spawn a CLEAN (all blockers terminal) ticket OR
    // a STACKED one — a child whose single in-flight blocker has an open ship to fork from. STACKED
    // starts early off the parent's branch (the producer cut the worktree there) instead of waiting
    // for the parent to merge; `tend_blocks` + the parent-targeted MR handle order + rebase-loop.
    let armada = ledger.armada()?;
    for order in ledger.queue()? {
        let ticket = match TicketRepo::get(ledger, &order.ticket)? {
            Some(t) => t,
            None => continue,
        };
        // A worker is frontier work — spawn only on a READY (To Do, blockers terminal) or STACKED
        // ticket. A hand is operator intent — spawn it wherever the operator dispatched it, whatever
        // the ticket's status (it's not frontier work; the operator drives it by hand).
        let spawnable = !order.auto_impl
            || is_ready(ledger, &ticket)?
            || matches!(classify(ledger, &ticket, &armada, stacks)?, Some(Classification::Stacked));
        if builder_due(ledger, &order)? && spawnable {
            let kind = if order.auto_impl { CrewKind::Worker } else { CrewKind::Hand };
            actions.push(Action::Spawn { ticket: order.ticket.0.clone(), kind: kind.as_str().into() });
        }
    }

    // Merged-ship REAP — iterate our ships (allies have no ticket).
    for ship in ledger.armada()? {
        let tkey = match &ship.ticket {
            Some(k) => k,
            None => continue,
        };
        let snap = match ShipRepo::snapshot(ledger, &ship.id)? {
            Some(s) => s,
            None => continue,
        };
        if snap.state == ShipState::Merged {
            // Reap while there's anything left to clean up: a live session to end, OR the open
            // order to retire. The cold sailor usually stands down (no live session) before the
            // merge lands, so the open order is the "not yet reaped" marker that drives Jira Done.
            let live = SessionRepo::for_ticket(ledger, tkey)?.iter().any(|s| s.ended_at.is_none());
            let order_open = OrderRepo::get(ledger, tkey)?.is_some();
            if live || order_open {
                actions.push(Action::Reap { ticket: tkey.0.clone() });
            }
        } else if matches!(snap.state, ShipState::Open | ShipState::Draft) {
            // LaunchSailor — the cold-loop sweep trigger. Two reasons to launch a sailor on an open
            // ship with no attendant: (1) it CHANGED since the last sweep (fingerprint ≠ swept), or
            // (2) it still NEEDS WORK — a conflict / open threads / failed pipeline that no sailor is
            // on. (2) is the safety net: fingerprint-change alone misses a broken-but-unchanged ship
            // whose escalated attendant was sunk or crashed. Loop-safe via the live-sailor lock — a
            // sailor that can't resolve a needy ship escalates and holds the slot, so it never re-
            // fires (a sailor must escalate, never sleep, on a needy ship — see sailor.md).
            // Gated to ships the fleet DISPATCHED as AUTO work: an order with `auto_impl` (a worker's
            // ship, landed by a cold-loop sailor) + a worktree to sweep in. A manual order (a hand,
            // operator-driven) or an un-dispatched ship gets NO sailor — the fleet doesn't land work
            // it didn't commission. Also gated: not held, and no live sailor already on it (the lock).
            // An open blocker does NOT gate the sweep — only the merge: `fleetc arm` refuses while the
            // ticket is blocked (the forge-neutral merge order, for forges without MR dependencies).
            // The blocked bit rides the fingerprint, so the blocker landing is itself a change: it
            // launches the sweep that arms.
            // Gated too: a ticket back in To Do (a rework) or with a builder at work is the builder's
            // — one cog in a worktree at a time; the handoff brings the sailor back.
            let auto = OrderRepo::get(ledger, tkey)?.is_some_and(|o| o.auto_impl && o.worktree_path.is_some());
            let ticket = TicketRepo::get(ledger, tkey)?;
            let held = ticket.as_ref().is_some_and(|t| t.held);
            let todo = ticket.as_ref().is_some_and(|t| t.status == Some(TicketStatus::Todo));
            let live: Vec<CrewKind> =
                SessionRepo::for_ticket(ledger, tkey)?.into_iter().filter(|s| s.ended_at.is_none()).map(|s| s.kind).collect();
            let live_sailor = live.contains(&CrewKind::Sailor);
            let live_builder = live.iter().any(|k| matches!(k, CrewKind::Worker | CrewKind::Hand));
            let changed = Some(ship_fingerprint(&snap, has_open_blocker(ledger, tkey)?)) != ship.swept;
            if auto && !held && !todo && !live_sailor && !live_builder && (needs_work(&snap) || changed) {
                actions.push(Action::LaunchSailor { ticket: tkey.0.clone() });
            }
        }
    }

    // TERMINAL-REAP with no cog left: a Done / Cancelled ticket (by the tracker, or a decommission)
    // whose order is still open reaps all the same — the order, the worktree. The pass below reaps
    // the ones with a live cog.
    for order in OrderRepo::queue(ledger)? {
        let status = TicketRepo::get(ledger, &order.ticket)?.and_then(|t| t.status);
        let terminal = matches!(status, Some(TicketStatus::Done) | Some(TicketStatus::Cancelled));
        let live = SessionRepo::for_ticket(ledger, &order.ticket)?.iter().any(|s| s.ended_at.is_none());
        let reap = Action::Reap { ticket: order.ticket.0.clone() };
        if terminal && !live && !actions.contains(&reap) {
            actions.push(reap);
        }
    }

    // HANDOFF + terminal-REAP + HEAL — per live cog, blending its report with liveness.
    for cog in cogs {
        // Ticketless cogs (captains, standing singletons) aren't part of the per-ticket
        // handoff/reap plan — skip them here.
        let Some(tkey) = &cog.session.ticket else { continue };
        let status = TicketRepo::get(ledger, tkey)?.and_then(|t| t.status);
        if matches!(status, Some(TicketStatus::Done) | Some(TicketStatus::Cancelled)) {
            actions.push(Action::Reap { ticket: tkey.0.clone() });
            continue;
        }
        let state = ReportRepo::latest(ledger, &cog.session.id)?.map(|r| r.state);
        // A builder that reported `concluded` → hand off to a sailor (unless one's live).
        if matches!(cog.session.kind, CrewKind::Worker | CrewKind::Hand)
            && state.is_some_and(|s| s.is_concluded())
        {
            let live_sailor = SessionRepo::for_ticket(ledger, tkey)?
                .iter()
                .any(|s| s.kind == CrewKind::Sailor && s.ended_at.is_none());
            if !live_sailor {
                actions.push(Action::Handoff { ticket: tkey.0.clone() });
            }
            continue;
        }
        // A sailor that reported `sleeping` swept clean and is now idling — still LIVE, because
        // `claude --agent` doesn't self-exit. This self-report is the graceful cold-loop reap:
        // stand it down (end the session + kill the idle tmux) so the sweep trigger re-launches
        // on the next change. Unlike the Dead path below, the cog is alive, so the StandDown's
        // host.kill does the real work. An operator anchor still outranks it.
        if cog.session.kind == CrewKind::Sailor
            && state.is_some_and(|s| s.is_done())
            && cog.session.anchored_at.is_none()
        {
            actions.push(Action::StandDown { ticket: tkey.0.clone(), kind: "sailor".into() });
            continue;
        }
        // Dead by liveness, with no deliberate-stop report → the host process is gone. A WARM
        // cog (worker/hand) is a crash → heal (respawn in place, resume from its status drop). A
        // COLD cog (sailor) has finished its one-shot watch — a clean exit and a crash look
        // identical (both Dead) → stand it down (end the session) so the sweep trigger re-launches
        // on the next change. An operator anchor is the authoritative hold: never touch an
        // anchored session, even if it died before self-reporting `anchored`.
        let paused = state.is_some_and(|s| s.is_paused());
        if cog.liveness == Liveness::Dead && !paused && cog.session.anchored_at.is_none() {
            let (ticket, kind) = (tkey.0.clone(), cog.session.kind.as_str().into());
            actions.push(if matches!(cog.session.kind, CrewKind::Worker | CrewKind::Hand) {
                Action::Heal { ticket, kind }
            } else {
                Action::StandDown { ticket, kind }
            });
        }
    }

    // CAPTAIN — the port of converge's `reconcile_foremen`, keyed on the convoy (ex board file):
    // one captain per ACTIVE convoy, COLD-looped. Unlike a worker, a captain is a one-shot — it
    // wakes, dispatches its convoy's ready tickets (or closes a fully-delivered convoy) and reports
    // `done`. `claude --agent` never exits by itself, so that report (or a Dead process) ends the
    // pass. The DB `one_live_captain_per_convoy` lock backstops a double spawn.
    let active = ConvoyRepo::active(ledger)?;
    // STAND DOWN a captain that finished its pass (`done` or Dead) or has no active convoy left to
    // command — unless the operator anchored it. Ending the session lets the trigger re-launch on
    // more work, in this same plan (the stand-down runs first and frees the slot).
    let mut stood_down = Vec::new();
    for cog in cogs {
        if cog.session.kind != CrewKind::Captain || cog.session.ended_at.is_some() {
            continue;
        }
        let Some(convoy) = &cog.session.convoy else { continue };
        let convoy_active = active.iter().any(|c| &c.key == convoy);
        let done = ReportRepo::latest(ledger, &cog.session.id)?.is_some_and(|r| r.state.is_done());
        if cog.session.anchored_at.is_none() && (done || cog.liveness == Liveness::Dead || !convoy_active) {
            actions.push(Action::StandDownCaptain { convoy: convoy.0.clone() });
            stood_down.push(convoy);
        }
    }
    // LAUNCH a captain for an active convoy that has a pass worth of work (a ready-undispatched
    // ticket, or every ticket terminal → close it) and no captain currently alive on it. An
    // operator-anchored convoy (its hold lives here, on the trigger source) is skipped — the
    // in-flight captain finishes gracefully, but no relaunch while the anchor stands.
    for convoy in &active {
        if convoy.anchored_at.is_some() {
            continue;
        }
        let has_live_captain = cogs.iter().any(|c| {
            c.session.kind == CrewKind::Captain
                && c.session.ended_at.is_none()
                && c.liveness != Liveness::Dead
                && c.session.convoy.as_ref() == Some(&convoy.key)
                && !stood_down.contains(&&convoy.key)
        });
        if !has_live_captain && convoy_has_work(ledger, &convoy.key)? {
            actions.push(Action::LaunchCaptain { convoy: convoy.key.0.clone() });
        }
    }

    // LOOKOUT — one SINGLETON cog reviewing ALLIED ships (MRs we don't own: no fleet order). Stand a
    // finished lookout down (it reported `reviewed`, or died); then, if none is live, launch it on
    // the next unreviewed ally. fleetd picks the iid — the first allied OPEN ship whose code changed
    // since its last review (head_sha ≠ reviewed) — and marks it reviewed at dispatch.
    let mut finished_lookout = false;
    let mut live_lookout = false;
    for cog in cogs {
        if cog.session.kind != CrewKind::Lookout || cog.session.ended_at.is_some() {
            continue;
        }
        let done = ReportRepo::latest(ledger, &cog.session.id)?.is_some_and(|r| r.state.is_done());
        if (done || cog.liveness == Liveness::Dead) && cog.session.anchored_at.is_none() {
            finished_lookout = true;
        } else {
            live_lookout = true; // alive (reviewing) or anchored → still holds the singleton slot
        }
    }
    if finished_lookout {
        actions.push(Action::StandDownLookout); // ended first, so the launch below can take the slot
    }
    // An operator-anchored lookout (its hold lives on the singleton `lookout` row, the trigger
    // source) is skipped — no relaunch while the anchor stands.
    if !live_lookout && !LookoutRepo::anchored(ledger)? {
        for ship in ledger.armada()? {
            // Ours = the fleet dispatched its ticket (an order). A peer's MR carries a ticket key too,
            // so the key alone says nothing about who owns it.
            let ours = match &ship.ticket {
                Some(t) => OrderRepo::get(ledger, t)?.is_some(),
                None => false,
            };
            if ours || ship.ended_at.is_some() {
                continue; // ours, or closed — the lookout reviews only allies
            }
            let unreviewed = match ShipRepo::snapshot(ledger, &ship.id)? {
                Some(snap) => {
                    snap.state == ShipState::Open && ship.reviewed.as_deref() != Some(snap.head_sha.as_str())
                }
                None => false,
            };
            if unreviewed {
                actions.push(Action::LaunchLookout { iid: ship.iid });
                break; // one review per pass
            }
        }
    }

    // De-dup identical actions (e.g. a merged ship + terminal ticket both → one Reap).
    let mut seen: Vec<Action> = Vec::new();
    actions.retain(|a| {
        if seen.contains(a) {
            false
        } else {
            seen.push(a.clone());
            true
        }
    });
    Ok(actions)
}

/// The MR dependencies to keep in step, per child MR: each of OUR open ships (we dispatched its
/// ticket) whose ticket has blockers, or whose MR the forge reports blocked, with the open ships of
/// its blockers. The blocker may be anyone's — our MR can wait on an ally's — but the fleet never
/// rewrites an ally's merge gate off its reading of the shared ticket graph. A cycle-closing edge
/// (the parent is blocked by the child) is skipped: GitLab accepts a cycle, then deadlocks. A child
/// left with no parent is still listed, so a blocker that closed unmerged is dropped. Pure.
pub(crate) fn wanted_blocks(ledger: &Ledger) -> Result<Vec<(i64, Vec<i64>)>> {
    let armada = ledger.armada()?;
    let open_ship = |ticket: &TicketKey| armada.iter().find(|s| s.ticket.as_ref() == Some(ticket));
    let mut wanted = Vec::new();
    for ticket in TicketRepo::board(ledger)? {
        let Some(child) = open_ship(&ticket.key) else { continue };
        if OrderRepo::get(ledger, &ticket.key)?.is_none() {
            continue; // not ours
        }
        let gated = ShipRepo::snapshot(ledger, &child.id)?.and_then(|s| s.merge_status).as_deref()
            == Some("merge_request_blocked");
        if ticket.blocked_by.is_empty() && !gated {
            continue;
        }
        let mut parents = Vec::new();
        for blocker in &ticket.blocked_by {
            let Some(parent) = open_ship(blocker) else { continue };
            if TicketRepo::get(ledger, blocker)?.is_some_and(|p| p.blocked_by.contains(&ticket.key)) {
                tracing::warn!("block !{}←!{}: skipped — {} and {} block each other", child.iid, parent.iid, ticket.key.0, blocker.0);
                continue;
            }
            parents.push(parent.iid);
        }
        wanted.push((child.iid, parents));
    }
    Ok(wanted)
}

/// Keep our MRs' dependencies in step with the ticket graph (`wanted_blocks`) through the forge.
/// Not a planned action: only the forge knows what's declared, so this asks it each tick and is
/// quiet when nothing changed. Best-effort per MR — a failure retries next tick.
fn tend_blocks<F: Forge + ?Sized>(ledger: &Ledger, forge: &F) -> Result<()> {
    for (child, parents) in wanted_blocks(ledger)? {
        match forge.sync_blocks(child, &parents) {
            Ok(0) => {}
            Ok(n) => tracing::info!("blocks of !{child}: {n} changed — now blocked by {parents:?}"),
            Err(e) => tracing::warn!("blocks of !{child} not tended: {e:#}"),
        }
    }
    Ok(())
}

/// Ready = To Do, not held, and every blocker is terminal (Done/Cancelled). The absorbed
/// `candidate` frontier — `pub(crate)` so the convoy view reuses the one definition. Kept
/// lightweight (no armada) for the hot spawn-gate path; the richer `classify` builds on it.
pub(crate) fn is_ready(ledger: &Ledger, ticket: &Ticket) -> Result<bool> {
    if ticket.held || !matches!(ticket.status, Some(TicketStatus::Todo)) {
        return Ok(false);
    }
    blockers_terminal(ledger, &ticket.blocked_by)
}

/// A builder is due on `order`: no cog has started on its ticket since it was dispatched (or
/// re-dispatched by `fleetc rework`).
fn builder_due(ledger: &Ledger, order: &Order) -> Result<bool> {
    let ordered = parse_time(&order.created_at);
    Ok(SessionRepo::for_ticket(ledger, &order.ticket)?.iter().all(|s| match (parse_time(&s.created_at), ordered) {
        (Some(started), Some(ordered)) => started < ordered,
        _ => s.created_at < order.created_at, // unparseable (test fixtures): compare as text
    }))
}

fn parse_time(at: &str) -> Option<time::OffsetDateTime> {
    time::OffsetDateTime::parse(at, &time::format_description::well_known::Rfc3339).ok()
}

/// Every blocker in `blockers` is terminal (Done/Cancelled) — the DAG-ready predicate shared by
/// `is_ready` (the ticket itself) and `classify` (a candidate's single parent, one level deep).
pub(crate) fn blockers_terminal(ledger: &Ledger, blockers: &[TicketKey]) -> Result<bool> {
    Ok(open_blockers(ledger, blockers)?.is_empty())
}

/// The blockers in `blockers` not yet Done/Cancelled (one the ledger doesn't know counts as open):
/// what still gates the merge, as `fleetc arm` and the packet report it.
pub(crate) fn open_blockers(ledger: &Ledger, blockers: &[TicketKey]) -> Result<Vec<TicketKey>> {
    let mut open = Vec::new();
    for b in blockers {
        let s = TicketRepo::get(ledger, b)?.and_then(|t| t.status);
        if !matches!(s, Some(TicketStatus::Done) | Some(TicketStatus::Cancelled)) {
            open.push(b.clone());
        }
    }
    Ok(open)
}

/// The ticket waits on a blocker that isn't Done/Cancelled yet (an unknown ticket has none).
fn has_open_blocker(ledger: &Ledger, key: &TicketKey) -> Result<bool> {
    match TicketRepo::get(ledger, key)? {
        Some(t) => Ok(!blockers_terminal(ledger, &t.blocked_by)?),
        None => Ok(false),
    }
}

/// fleetd's dispatch verdict for a convoy ticket — the absorbed `classify.py`. `None` for a ticket
/// past the frontier (not To Do). CLEAN reuses `is_ready`; STACKED is a single non-terminal blocker
/// that is in-flight (an open ship to fork from) and itself one level deep; anything else
/// non-dispatchable is BLOCKED; a held ticket is HOLD; `stacks` = the forge can stack (else no
/// STACKED verdict). `pub(crate)` for the convoy view.
pub(crate) fn classify(ledger: &Ledger, ticket: &Ticket, armada: &[Ship], stacks: bool) -> Result<Option<Classification>> {
    if !matches!(ticket.status, Some(TicketStatus::Todo)) {
        return Ok(None); // past the dispatch frontier
    }
    if ticket.held {
        return Ok(Some(Classification::Hold));
    }
    // Which blockers aren't terminal yet?
    let mut pending = Vec::new();
    for b in &ticket.blocked_by {
        let s = TicketRepo::get(ledger, b)?.and_then(|t| t.status);
        if !matches!(s, Some(TicketStatus::Done) | Some(TicketStatus::Cancelled)) {
            pending.push(b);
        }
    }
    if pending.is_empty() {
        return Ok(Some(Classification::Clean));
    }
    // STACKED: exactly one non-terminal blocker, in-flight (an open ship to fork from) and itself
    // one level deep (its own blockers all terminal — not a deeper stack). Else BLOCKED. A forge
    // that can't stack (no MR dependencies) never stacks: the child waits for its parent to merge.
    if stacks && pending.len() == 1 {
        let parent = pending[0];
        let in_flight = armada.iter().any(|s| s.ticket.as_ref() == Some(parent) && s.ended_at.is_none());
        let one_level = match TicketRepo::get(ledger, parent)? {
            Some(p) => blockers_terminal(ledger, &p.blocked_by)?,
            None => false,
        };
        if in_flight && one_level {
            return Ok(Some(Classification::Stacked));
        }
    }
    Ok(Some(Classification::Blocked))
}

/// A convoy has a pass worth of captain work iff some ticket is READY but un-dispatched (there's a
/// dispatch to make) OR every ticket is terminal (the convoy is delivered — time to close it). When
/// the tickets are merely in-flight (dispatched, not yet terminal) the captain sleeps. Shares the
/// `is_ready` + open-order frontier with the `ConvoyView` the captain reads, so the cold trigger
/// and the view agree on what "ready to dispatch" means.
fn convoy_has_work(ledger: &Ledger, convoy: &ConvoyKey) -> Result<bool> {
    let tickets: Vec<Ticket> = TicketRepo::board(ledger)?
        .into_iter()
        .filter(|t| t.convoy.as_ref() == Some(convoy))
        .collect();
    if tickets.is_empty() {
        return Ok(false);
    }
    let terminal =
        |t: &Ticket| matches!(t.status, Some(TicketStatus::Done) | Some(TicketStatus::Cancelled));
    if tickets.iter().all(terminal) {
        return Ok(true); // convoy delivered → close it
    }
    for t in &tickets {
        if is_ready(ledger, t)? && OrderRepo::get(ledger, &t.key)?.is_none() {
            return Ok(true); // a ready, un-dispatched ticket → dispatch it
        }
    }
    Ok(false)
}

/// Apply the reconcile plan through the ports. `dry_run=true` logs the plan and executes
/// nothing (identical to `tick_dry_run`); `dry_run=false` applies each action, recording
/// what ran and what failed — a single action failing is never fatal, the next tick
/// retries (converge's posture). Returns the count applied (dry-run: the count planned).
pub fn tick<T: Tracker + ?Sized, H: SessionHost, F: Forge + ?Sized>(
    ledger: &Ledger,
    tracker: &T,
    host: &H,
    forge: &F,
    cogs: &[Cog],
    project: &str,
    dry_run: bool,
) -> Result<usize> {
    let actions = plan(ledger, cogs, forge.stacks())?;
    if dry_run {
        ledger.append(
            &now_rfc3339(),
            EventKind::TickRan,
            EntityKind::Ticker,
            "reconcile",
            &serde_json::json!({ "dry_run": true, "planned": actions }),
        )?;
        return Ok(actions.len());
    }

    let mut applied: Vec<Action> = Vec::new();
    let mut failed: Vec<serde_json::Value> = Vec::new();
    for action in &actions {
        match execute(action, ledger, tracker, host, forge, project) {
            Ok(()) => applied.push(action.clone()),
            Err(e) => failed.push(serde_json::json!({ "action": action, "error": format!("{e:#}") })),
        }
    }
    if forge.stacks() {
        tend_blocks(ledger, forge)?;
    }
    ledger.append(
        &now_rfc3339(),
        EventKind::TickRan,
        EntityKind::Ticker,
        "reconcile",
        &serde_json::json!({ "dry_run": false, "applied": applied, "failed": failed }),
    )?;
    Ok(applied.len())
}

/// Dispatch one planned action to its port-driven effect. Spawn is live; handoff/reap/heal
/// land in their own increments.
fn execute<T: Tracker + ?Sized, H: SessionHost, F: Forge + ?Sized>(
    action: &Action,
    ledger: &Ledger,
    tracker: &T,
    host: &H,
    forge: &F,
    project: &str,
) -> Result<()> {
    match action {
        Action::Spawn { ticket, kind } => spawn(ledger, tracker, host, ticket, kind),
        Action::Handoff { ticket } => handoff(ledger, tracker, host, forge, ticket),
        Action::Reap { ticket } => reap(ledger, tracker, host, project, ticket),
        Action::Heal { ticket, kind } => heal(ledger, host, ticket, kind),
        Action::StandDown { ticket, kind } => stand_down(ledger, host, ticket, kind),
        Action::LaunchCaptain { convoy } => launch_captain(ledger, host, project, convoy),
        Action::StandDownCaptain { convoy } => stand_down_captain(ledger, host, convoy),
        Action::LaunchSailor { ticket } => launch_sailor(ledger, forge, host, ticket),
        Action::LaunchLookout { iid } => launch_lookout(ledger, forge, host, project, *iid),
        Action::StandDownLookout => stand_down_lookout(ledger, host),
    }
}

/// Spawn — the live counterpart to converge's `spawn_workers`: read the producer's worktree
/// off the order, claim Jira (To Do → In Progress), record the session, launch the cog, and
/// emit `SessionStarted`. A missing worktree aborts the launch (recorded as a failure — the
/// producer must materialise it), mirroring converge's `dispatch_missing_worktree`. Jira is
/// claimed AFTER the worktree check, so a missing worktree never moves the ticket.
fn spawn<T: Tracker + ?Sized, H: SessionHost>(
    ledger: &Ledger,
    tracker: &T,
    host: &H,
    ticket: &str,
    kind: &str,
) -> Result<()> {
    let tkey = TicketKey(ticket.to_string());
    let order = OrderRepo::get(ledger, &tkey)?
        .ok_or_else(|| anyhow!("spawn {ticket}: no order on the queue"))?;
    let worktree = order.worktree_path.ok_or_else(|| {
        anyhow!("spawn {ticket}: order has no worktree_path (the producer must materialise it)")
    })?;
    let crew = CrewKind::parse(kind).ok_or_else(|| anyhow!("spawn {ticket}: unknown crew kind {kind:?}"))?;

    // Claim gate (claim.sh step 0): never stomp a peer. If a teammate owns it, skip the spawn —
    // benign (the order stays queued; a later tick re-checks). A gate *error* is fatal (fail
    // closed: don't claim if we can't verify ownership).
    if tracker.assigned_to_peer(&tkey)? {
        tracing::info!("spawn {ticket}: assigned to a peer — skipping the claim (respecting it)");
        return Ok(());
    }

    // The claim ceremony is WORKER-only — frontier bookkeeping that marks a To-Do ticket as
    // active fleet work. A HAND is operator intent on a (usually in-flight) ticket the operator
    // owns: it must not move the ticket's lane, re-label, or re-sprint. A hand just launches.
    if crew == CrewKind::Worker {
        // Claim Jira only after the worktree check — a missing worktree never moves the ticket.
        // The transition is the lock (claim.sh step 1): a peer who moved it past To Do fails here.
        tracker.transition(&tkey, TicketStatus::InProgress)?;

        // Claim follow-ons, best-effort: a failure is a partial claim — logged, not fatal (the
        // transition already locked the ticket and the cog must launch).
        if let Err(e) = tracker.claim(&tkey) {
            tracing::warn!("spawn {ticket}: claim follow-ons failed: {e:#}");
        }
    }

    let tmux_session = format!("{kind}-{ticket}");
    let hid = new_session_id();
    let session_id = match SessionRepo::start(
        ledger,
        &Session {
            id: SessionId(0),
            kind: crew,
            ticket: Some(tkey),
            convoy: None,
            worktree_path: worktree.clone(),
            tmux_session: tmux_session.clone(),
            harness_session_id: Some(hid.clone()),
            created_at: now_rfc3339(),
            ended_at: None,
            anchored_at: None,
        },
    )? {
        Some(id) => id,
        // The lock already holds the slot — a live cog of this kind exists. Benign skip.
        None => return Ok(()),
    };

    host.spawn(&tmux_session, Path::new(&worktree), &ticket_launch_cmd(kind, ticket, Launch::Fresh(&hid)))?;

    ledger.append(
        &now_rfc3339(),
        EventKind::SessionStarted,
        EntityKind::Session,
        &session_id.0.to_string(),
        &serde_json::json!({ "ticket": ticket, "kind": kind }),
    )?;
    Ok(())
}

/// Launch a TICKETLESS hand in `worktree` — an operator-driven scratch cog with no ticket and no
/// order (the `fleetc hand` path). It records a tracked session (dashboard-visible, liveness-probed)
/// and launches `claude --agent hand`. reconcile leaves ticketless cogs alone, so the operator owns
/// its lifecycle. The tmux name is the worktree's basename (`hand-<dir>`) — one hand per worktree;
/// a dup is refused up front so we never leave an orphan session row behind a failed tmux spawn.
pub(crate) fn spawn_ticketless_hand<H: SessionHost>(ledger: &Ledger, host: &H, worktree: &str) -> Result<()> {
    let base = Path::new(worktree).file_name().and_then(|s| s.to_str()).unwrap_or("hand");
    let tmux_session = format!("hand-{base}");
    if host.alive(&tmux_session)? {
        return Err(anyhow!("a hand is already running in {worktree} (tmux {tmux_session})"));
    }
    let hid = new_session_id();
    let session_id = SessionRepo::start(
        ledger,
        &Session {
            id: SessionId(0),
            kind: CrewKind::Hand,
            ticket: None,
            convoy: None,
            worktree_path: worktree.to_string(),
            tmux_session: tmux_session.clone(),
            harness_session_id: Some(hid.clone()),
            created_at: now_rfc3339(),
            ended_at: None,
            anchored_at: None,
        },
    )?
    .ok_or_else(|| anyhow!("spawn_ticketless_hand: session not recorded"))?;

    host.spawn(&tmux_session, Path::new(worktree), &launch_cmd("hand", Launch::Fresh(&hid)))?;

    ledger.append(
        &now_rfc3339(),
        EventKind::SessionStarted,
        EntityKind::Session,
        &session_id.0.to_string(),
        &serde_json::json!({ "ticket": serde_json::Value::Null, "kind": "hand" }),
    )?;
    Ok(())
}

/// Handoff — the live counterpart to converge's `spawn_finishers`: the builder reported
/// `concluded`, so move Jira In Progress → In QA, hand the ready MR to the team's review, then reap
/// the builder (end its session + kill its tmux, best-effort — the builder may already be dead,
/// which is a finish, not an error). The sailor is NOT launched here — `LaunchSailor` owns that,
/// firing once the ship appears in the armada sync.
fn handoff<T: Tracker + ?Sized, H: SessionHost, F: Forge + ?Sized>(
    ledger: &Ledger,
    tracker: &T,
    host: &H,
    forge: &F,
    ticket: &str,
) -> Result<()> {
    let tkey = TicketKey(ticket.to_string());
    let sessions = SessionRepo::for_ticket(ledger, &tkey)?;
    let builder = sessions
        .iter()
        .find(|s| matches!(s.kind, CrewKind::Worker | CrewKind::Hand) && s.ended_at.is_none())
        .ok_or_else(|| anyhow!("handoff {ticket}: no live builder session"))?;

    // 0. If the operator drove this by HAND, promote its manual order to AUTO so LaunchSailor
    //    engages a sailor to sail the now-ready MR to merge — the "pawn it off" handoff. (A worker's
    //    order is already auto, so this is a no-op there.)
    if builder.kind == CrewKind::Hand {
        if let Some(order) = OrderRepo::get(ledger, &tkey)?.filter(|o| !o.auto_impl) {
            OrderRepo::dispatch(ledger, &Order { auto_impl: true, ..order })?;
        }
    }

    // 1. Move Jira In Progress → In QA.
    tracker.transition(&tkey, TicketStatus::InQa)?;

    // 2. Mark the MR ready (a draft is the pre-review state) + request the team's review — once,
    //    here. Best-effort: a forge hiccup
    //    (or an MR not yet queryable) must not lose the In-QA transition + reap. The forge resolves
    //    the iid from the ticket's branch, so the ship needn't be in our ledger yet.
    match forge.request_review(&tkey) {
        Ok(reviewers) if reviewers.is_empty() => {
            tracing::info!("handoff {ticket}: marked ready; review request skipped (the forge requests reviewers itself)")
        }
        Ok(reviewers) => tracing::info!("handoff {ticket}: review requested from {reviewers:?}"),
        Err(e) => tracing::warn!("handoff {ticket}: review request failed (non-fatal): {e:#}"),
    }

    // 3. Reap the builder: end its session + kill its tmux (best-effort).
    SessionRepo::end(ledger, &builder.id, &now_rfc3339())?;
    let _ = host.kill(&builder.tmux_session);
    ledger.append(
        &now_rfc3339(),
        EventKind::SessionEnded,
        EntityKind::Session,
        &builder.id.0.to_string(),
        &serde_json::json!({ "ticket": ticket, "kind": builder.kind.as_str(), "reason": "handoff" }),
    )?;

    Ok(())
}

/// Reap — the live counterpart to converge's `conclude_dispatches`: the work is terminal
/// (a merged ship, or a Done/Cancelled ticket), so drive Jira to Done unless it's already
/// terminal (the merged-ship case moves In QA → Done; a terminal ticket is idempotently
/// skipped), end every live session + kill its tmux, reclaim the now-dead worktree, and retire the
/// order (the fulfilled dispatch intent — converge's "reap packet").
pub(crate) fn reap<T: Tracker + ?Sized, H: SessionHost>(
    ledger: &Ledger,
    tracker: &T,
    host: &H,
    project: &str,
    ticket: &str,
) -> Result<()> {
    let tkey = TicketKey(ticket.to_string());
    // The worktree to reclaim — read off the order BEFORE we retire it below.
    let worktree = OrderRepo::get(ledger, &tkey)?.and_then(|o| o.worktree_path);

    let status = TicketRepo::get(ledger, &tkey)?.and_then(|t| t.status);
    if !matches!(status, Some(TicketStatus::Done) | Some(TicketStatus::Cancelled)) {
        tracker.transition(&tkey, TicketStatus::Done)?;
    }

    // rollup_parent: the ticket is now Done; if it's a sub-task and ALL its siblings are
    // terminal, close the empty parent container. Best-effort — a tracker hiccup or a redundant
    // close (two last siblings reaping together) never fails the reap.
    if let Err(e) = roll_up_parent(tracker, &tkey) {
        tracing::warn!("reap {ticket}: parent rollup failed: {e:#}");
    }

    for session in SessionRepo::for_ticket(ledger, &tkey)? {
        if session.ended_at.is_some() {
            continue;
        }
        SessionRepo::end(ledger, &session.id, &now_rfc3339())?;
        let _ = host.kill(&session.tmux_session);
        ledger.append(
            &now_rfc3339(),
            EventKind::SessionEnded,
            EntityKind::Session,
            &session.id.0.to_string(),
            &serde_json::json!({ "ticket": ticket, "kind": session.kind.as_str(), "reason": "reap" }),
        )?;
    }

    // Worktree hygiene: the ticket is done, so its checkout is a corpse — reclaim it (+ its branch)
    // now that no cog is using it. Best-effort: a removal failure (e.g. a dirty tree) must not fail
    // the reap. Guarded on existence — idempotent, and a no-op in tests (fake paths don't exist).
    if let Some(wt) = worktree {
        if std::path::Path::new(&wt).exists() {
            if let Err(e) = crate::git::GitWorktrees::new(project).remove(&wt) {
                tracing::warn!("reap {ticket}: worktree cleanup failed (non-fatal): {e:#}");
            }
        }
    }

    // Retire the order: the dispatch intent is fulfilled (a no-op if there's none).
    OrderRepo::cancel(ledger, &tkey)?;

    // End the ticket's ships that left the open list (merged, or closed): nothing waits on them now.
    for ship in ledger.armada()?.into_iter().filter(|s| s.ticket.as_ref() == Some(&tkey)) {
        let done = ShipRepo::snapshot(ledger, &ship.id)?.is_some_and(|s| matches!(s.state, ShipState::Merged | ShipState::Closed));
        if done {
            ShipRepo::retire(ledger, &ship.id, &now_rfc3339())?;
        }
    }
    Ok(())
}

/// converge's `rollup_parent`: a sub-task just reaped (Done); if ALL of its parent's sub-tasks are
/// terminal, close the empty parent container (the parent carries no work of its own). A no-op for
/// a non-sub-task (`parent_of` → None) or an Epic (`subtasks` empty — the guard). Fully live: the
/// completeness check must be authoritative, so it can't be cached off the sprint-scoped ledger.
fn roll_up_parent<T: Tracker + ?Sized>(tracker: &T, child: &TicketKey) -> Result<()> {
    let Some(parent) = tracker.parent_of(child)? else { return Ok(()) };
    let subs = tracker.subtasks(&parent)?;
    let all_done =
        !subs.is_empty() && subs.iter().all(|s| matches!(s, TicketStatus::Done | TicketStatus::Cancelled));
    if all_done {
        tracker.transition(&parent, TicketStatus::Done)?;
    }
    Ok(())
}

/// Heal — the live counterpart to converge's `reconcile_cogs` respawn: a live session whose
/// host process is gone (`Liveness::Dead`) with no deliberate-stop report is a crash. Relaunch
/// its tmux in place; the session row stays, so the cog resumes from its status drop (Restart
/// Awareness). No new session row, so no `SessionStarted` — the `TickRan` applied[] records it.
/// Safe because Dead means the tmux is gone (a stale-but-alive session classifies as Parked,
/// never Dead), so the respawn can't collide with a live session.
fn heal<H: SessionHost>(ledger: &Ledger, host: &H, ticket: &str, kind: &str) -> Result<()> {
    let tkey = TicketKey(ticket.to_string());
    let crew = CrewKind::parse(kind).ok_or_else(|| anyhow!("heal {ticket}: unknown crew kind {kind:?}"))?;
    let sessions = SessionRepo::for_ticket(ledger, &tkey)?;
    let session = sessions
        .iter()
        .find(|s| s.kind == crew && s.ended_at.is_none())
        .ok_or_else(|| anyhow!("heal {ticket}: no live {kind} session to respawn"))?;
    // Relaunch on the session's OWN id so the transcript stays one continuous file across the crash.
    let cmd = ticket_launch_cmd(kind, ticket, Launch::relaunch(session));
    host.spawn(&session.tmux_session, Path::new(&session.worktree_path), &cmd)?;
    Ok(())
}

/// StandDown — the cold-loop counterpart to heal: a cold cog (sailor) whose host process is gone
/// has finished its one-shot watch (a clean exit and a crash look identical — both Dead), so end
/// its session + kill its tmux (best-effort; already dead). No respawn — the sweep trigger re-
/// launches on the next change. No Jira, no order: a stood-down sailor's work lives on in its MR,
/// reaped only when the ship merges.
fn stand_down<H: SessionHost>(ledger: &Ledger, host: &H, ticket: &str, kind: &str) -> Result<()> {
    let tkey = TicketKey(ticket.to_string());
    let crew = CrewKind::parse(kind).ok_or_else(|| anyhow!("stand_down {ticket}: unknown crew kind {kind:?}"))?;
    let sessions = SessionRepo::for_ticket(ledger, &tkey)?;
    let session = sessions
        .iter()
        .find(|s| s.kind == crew && s.ended_at.is_none())
        .ok_or_else(|| anyhow!("stand_down {ticket}: no live {kind} session"))?;
    SessionRepo::end(ledger, &session.id, &now_rfc3339())?;
    let _ = host.kill(&session.tmux_session);
    ledger.append(
        &now_rfc3339(),
        EventKind::SessionEnded,
        EntityKind::Session,
        &session.id.0.to_string(),
        &serde_json::json!({ "ticket": ticket, "kind": kind, "reason": "stand_down" }),
    )?;
    Ok(())
}

/// LaunchCaptain — the live counterpart to converge's `reconcile_foremen` spawn: launch a captain
/// in the project checkout (no ticket, no worktree, no Jira), record the ticketless session, and emit
/// `SessionStarted`. The captain self-identifies its convoy from the `FLEET_CONVOY` env var — an
/// adapter-agnostic contract (no tmux/vendor leak into the agent), injected as a cmd-prefix so
/// `TmuxHost` stays generic. `Ok(None)` from the `one_live_captain_per_convoy` lock means the slot
/// is already held → a benign skip.
fn launch_captain<H: SessionHost>(ledger: &Ledger, host: &H, project: &str, convoy: &str) -> Result<()> {
    let tmux_session = format!("captain-{convoy}");
    let hid = new_session_id();
    let session_id = match SessionRepo::start(
        ledger,
        &Session {
            id: SessionId(0),
            kind: CrewKind::Captain,
            ticket: None,
            convoy: Some(ConvoyKey(convoy.to_string())),
            worktree_path: project.to_string(),
            tmux_session: tmux_session.clone(),
            harness_session_id: Some(hid.clone()),
            created_at: now_rfc3339(),
            ended_at: None,
            anchored_at: None,
        },
    )? {
        Some(id) => id,
        None => return Ok(()),
    };

    let cmd = format!("FLEET_CONVOY={convoy} {}", launch_cmd("captain", Launch::Fresh(&hid)));
    host.spawn(&tmux_session, Path::new(project), &cmd)?;

    ledger.append(
        &now_rfc3339(),
        EventKind::SessionStarted,
        EntityKind::Session,
        &session_id.0.to_string(),
        &serde_json::json!({ "convoy": convoy, "kind": "captain" }),
    )?;
    Ok(())
}

/// StandDownCaptain — the cold-loop counterpart for captains (cf. `stand_down` for sailors): the
/// captain finished its pass (reported `done`, or Dead) or its convoy is gone, so end its session + kill its tmux
/// (best-effort — it may already be dead, a finish not an error). No Jira, no order: a captain owns
/// neither. The trigger re-launches on the convoy's next pass worth of work.
fn stand_down_captain<H: SessionHost>(ledger: &Ledger, host: &H, convoy: &str) -> Result<()> {
    let ckey = ConvoyKey(convoy.to_string());
    for session in SessionRepo::live(ledger)? {
        if session.kind != CrewKind::Captain || session.convoy.as_ref() != Some(&ckey) {
            continue;
        }
        SessionRepo::end(ledger, &session.id, &now_rfc3339())?;
        let _ = host.kill(&session.tmux_session);
        ledger.append(
            &now_rfc3339(),
            EventKind::SessionEnded,
            EntityKind::Session,
            &session.id.0.to_string(),
            &serde_json::json!({ "convoy": convoy, "kind": "captain", "reason": "stand_down" }),
        )?;
    }
    Ok(())
}

/// The change-fingerprint of a ship snapshot — the fields a sailor would act on (state, head,
/// conflicts, threads, approvals, pipeline, merge-readiness). Excludes `synced_at` (always moves) +
/// the defaulted arm fields, so it changes only when something sweep-worthy changed. `merge_status`
/// is in here so a readiness change (ci_must_pass → mergeable, mergeable → conflict) re-fires the sailor.
/// Does this ship have open work a sailor must act on THIS sweep? The "needs attention" half of
/// the LaunchSailor trigger (beside fingerprint-change), so a broken-but-unchanged ship with no
/// attendant still gets one. The signals are the ones `sailor.md` acts on: a conflict (rechart),
/// open review threads (parley), or a failed pipeline (repair). A ship gated on an upstream MR
/// (`merge_request_blocked`) is waiting, not working — it re-engages via a fingerprint change when
/// the block clears, so exclude it to avoid churning sailors on an unfixable-until-upstream failure.
/// A clean ship is resting: its first arming sweep is driven by fingerprint-change on handoff, not
/// here, so `needs_work` stays silent on it (no churn on an armed-and-waiting ship).
fn needs_work(s: &ShipSnapshot) -> bool {
    if s.merge_status.as_deref() == Some("merge_request_blocked") {
        return false;
    }
    s.has_conflicts || s.open_threads > 0 || s.pipeline == Some(PipelineStatus::Failed)
}

/// What a sweep was launched against. `blocked` (an open ticket blocker) appends a marker only when
/// set, so an unblocked ship's fingerprint is unchanged by it and the blocker landing reads as a change.
fn ship_fingerprint(s: &ShipSnapshot, blocked: bool) -> String {
    format!(
        "{:?}|{}|{}|{}|{}/{}|{:?}|{:?}{}",
        s.state,
        s.head_sha,
        s.has_conflicts,
        s.open_threads,
        s.approvals.current,
        s.approvals.required,
        s.pipeline,
        s.merge_status,
        if blocked { "|blocked" } else { "" },
    )
}

/// Fresh-poll one ship (by iid) so a cog's packet carries current state; fall back to the last sync
/// on a poll failure. Shared by the sailor + lookout launches.
pub(crate) fn fresh_snapshot<F: Forge + ?Sized>(ledger: &Ledger, forge: &F, ship: &Ship) -> Result<ShipSnapshot> {
    match forge.snapshot(ship.iid) {
        Ok(fs) => {
            let snap = ShipSnapshot {
                ship: ship.id.clone(),
                title: fs.title,
                state: fs.state,
                head_sha: fs.head_sha,
                has_conflicts: fs.has_conflicts,
                open_threads: fs.open_threads,
                approvals: fs.approvals,
                armed: fs.armed,
                on_merge_train: fs.on_merge_train,
                pipeline: fs.pipeline,
                merge_status: fs.merge_status,
                synced_at: now_rfc3339(),
            };
            ShipRepo::sync_snapshot(ledger, &snap)?;
            Ok(snap)
        }
        Err(e) => {
            tracing::warn!("fresh poll of !{} failed, using last sync: {e:#}", ship.iid);
            ShipRepo::snapshot(ledger, &ship.id)?
                .ok_or_else(|| anyhow!("ship {}: no snapshot", ship.iid))
        }
    }
}

/// LaunchSailor — the cold-loop sweep: fresh-poll this one ship so the sailor's packet carries
/// current state, record the fingerprint we dispatched against (`swept`), then launch a one-shot
/// sailor in the ship's worktree (off the order, which outlives the reaped worker). The
/// `one_live_crew_per_ticket` lock makes a duplicate a benign skip.
fn launch_sailor<H: SessionHost, F: Forge + ?Sized>(
    ledger: &Ledger,
    forge: &F,
    host: &H,
    ticket: &str,
) -> Result<()> {
    let tkey = TicketKey(ticket.to_string());
    let ship = ledger
        .armada()?
        .into_iter()
        .find(|s| s.ticket.as_ref() == Some(&tkey) && s.ended_at.is_none())
        .ok_or_else(|| anyhow!("launch_sailor {ticket}: no open ship"))?;

    let snap = fresh_snapshot(ledger, forge, &ship)?;
    ShipRepo::set_swept(ledger, &ship.id, Some(&ship_fingerprint(&snap, has_open_blocker(ledger, &tkey)?)))?;

    let worktree = OrderRepo::get(ledger, &tkey)?
        .and_then(|o| o.worktree_path)
        .ok_or_else(|| anyhow!("launch_sailor {ticket}: order has no worktree_path"))?;

    let sailor_tmux = format!("sailor-{ticket}");
    let hid = new_session_id();
    let session_id = match SessionRepo::start(
        ledger,
        &Session {
            id: SessionId(0),
            kind: CrewKind::Sailor,
            ticket: Some(tkey),
            convoy: None,
            worktree_path: worktree.clone(),
            tmux_session: sailor_tmux.clone(),
            harness_session_id: Some(hid.clone()),
            created_at: now_rfc3339(),
            ended_at: None,
            anchored_at: None,
        },
    )? {
        Some(id) => id,
        None => return Ok(()), // a live sailor already holds the lock — benign skip
    };

    host.spawn(&sailor_tmux, Path::new(&worktree), &ticket_launch_cmd("sailor", ticket, Launch::Fresh(&hid)))?;
    ledger.append(
        &now_rfc3339(),
        EventKind::SessionStarted,
        EntityKind::Session,
        &session_id.0.to_string(),
        &serde_json::json!({ "ticket": ticket, "kind": "sailor" }),
    )?;
    Ok(())
}

/// LaunchLookout — the SINGLETON allied-ship review cold-loop. Review ONE allied ship (an MR we don't own).
/// fleetd has picked the iid (the next unreviewed ally); fresh-poll it, mark it `reviewed = head_sha`
/// at dispatch (sailor-style — the head this pass reviews), then launch the one lookout in the project checkout
/// with the MR's iid injected via `FLEET_SHIP` (no worktree — an allied MR isn't ours; `/mr-review`
/// fetches by SHA). The `one_live_lookout` lock makes a duplicate a benign skip.
fn launch_lookout<H: SessionHost, F: Forge + ?Sized>(
    ledger: &Ledger,
    forge: &F,
    host: &H,
    project: &str,
    iid: i64,
) -> Result<()> {
    let ship = ledger
        .armada()?
        .into_iter()
        .find(|s| s.iid == iid && s.ended_at.is_none())
        .ok_or_else(|| anyhow!("launch_lookout !{iid}: no such ship"))?;

    let snap = fresh_snapshot(ledger, forge, &ship)?;
    ShipRepo::set_reviewed(ledger, &ship.id, &snap.head_sha)?;

    let lookout_tmux = "lookout".to_string();
    let hid = new_session_id();
    let session_id = match SessionRepo::start(
        ledger,
        &Session {
            id: SessionId(0),
            kind: CrewKind::Lookout,
            ticket: None,
            convoy: None,
            worktree_path: project.to_string(),
            tmux_session: lookout_tmux.clone(),
            harness_session_id: Some(hid.clone()),
            created_at: now_rfc3339(),
            ended_at: None,
            anchored_at: None,
        },
    )? {
        Some(id) => id,
        None => return Ok(()), // the singleton lock holds → a lookout is already reviewing. Benign skip.
    };

    let cmd = format!("FLEET_SHIP={iid} {}", launch_cmd("lookout", Launch::Fresh(&hid)));
    host.spawn(&lookout_tmux, Path::new(project), &cmd)?;
    ledger.append(
        &now_rfc3339(),
        EventKind::SessionStarted,
        EntityKind::Session,
        &session_id.0.to_string(),
        &serde_json::json!({ "iid": iid, "kind": "lookout" }),
    )?;
    Ok(())
}

/// StandDownLookout — the singleton lookout finished its one review (or crashed): end its session +
/// kill its tmux (best-effort; already dead). The trigger re-launches on the next unreviewed ally.
fn stand_down_lookout<H: SessionHost>(ledger: &Ledger, host: &H) -> Result<()> {
    for session in SessionRepo::live(ledger)? {
        if session.kind != CrewKind::Lookout {
            continue;
        }
        SessionRepo::end(ledger, &session.id, &now_rfc3339())?;
        let _ = host.kill(&session.tmux_session);
        ledger.append(
            &now_rfc3339(),
            EventKind::SessionEnded,
            EntityKind::Session,
            &session.id.0.to_string(),
            &serde_json::json!({ "kind": "lookout", "reason": "stand_down" }),
        )?;
    }
    Ok(())
}

/// How a cog's claude starts. fleetd mints the session id, so the transcript lands at a known
/// path (`Session::transcript`) that `fleetc transcript` resolves directly — no guessing.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Launch<'a> {
    /// A new conversation under this id (`--session-id`; claude refuses it if the transcript exists).
    Fresh(&'a str),
    /// Continue this id's existing transcript (`--resume`; claude refuses it if there is none).
    Resume(&'a str),
    /// No stored id — a heal of a pre-capture session; its transcript resolves by convention.
    Plain,
}

impl<'a> Launch<'a> {
    /// A heal's relaunch: resume the cog's own transcript when it exists, else start that id fresh
    /// (the cog died before claude wrote anything).
    fn relaunch(session: &'a domain::Session) -> Self {
        match session.harness_session_id.as_deref() {
            Some(id) if session.transcript().is_some_and(|t| t.exists()) => Launch::Resume(id),
            Some(id) => Launch::Fresh(id),
            None => Launch::Plain,
        }
    }
}

/// The command a cog runs inside its session — `claude <launch> --agent <kind>`. `TmuxHost` runs
/// it in a detached tmux session + injects the per-cog OTEL attribution (see tmux.rs).
fn launch_cmd(kind: &str, launch: Launch) -> String {
    match launch {
        Launch::Fresh(id) => format!("claude --session-id {id} --agent {kind}"),
        Launch::Resume(id) => format!("claude --resume {id} --agent {kind}"),
        Launch::Plain => format!("claude --agent {kind}"),
    }
}

/// `launch_cmd` for a ticketed cog (worker / sailor / hand): the ticket rides in as `FLEET_TICKET`
/// (as a captain's convoy rides in `FLEET_CONVOY`), so the cog never infers it from its branch.
/// The key is `<PREFIX>-<n>`-validated on the way in, so it is shell-safe here.
fn ticket_launch_cmd(kind: &str, ticket: &str, launch: Launch) -> String {
    format!("FLEET_TICKET={ticket} {}", launch_cmd(kind, launch))
}

/// Mint a fresh harness session id — a v4 UUID fleetd hands to `claude --session-id` so the
/// cog's transcript is findable at a deterministic path. Stored on the `Session` row at spawn.
fn new_session_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::ports::EventLog;
    use domain::*;
    use std::cell::RefCell;

    fn db() -> Ledger {
        Ledger::in_memory().unwrap()
    }

    fn ticket(db: &Ledger, key: &str, status: TicketStatus, held: bool, blocked_by: Vec<&str>) {
        let edges: Vec<TicketKey> = blocked_by.iter().map(|b| TicketKey((*b).into())).collect();
        TicketRepo::upsert(
            db,
            &Ticket {
                key: TicketKey(key.into()),
                convoy: None,
                held,
                created_at: "t".into(),
                blocked_by: edges.clone(),
                title: None,
                assignee: None,
                status: Some(status),
                synced_at: None,
            },
        )
        .unwrap();
        if !edges.is_empty() {
            TicketRepo::set_blocked_by(db, &TicketKey(key.into()), &edges).unwrap();
        }
    }

    fn order(db: &Ledger, key: &str, auto_impl: bool) {
        order_wt(db, key, auto_impl, Some("/wt"));
    }

    /// A ticket tagged into a convoy (the per-convoy frontier the captain trigger reads).
    fn convoy_ticket(db: &Ledger, key: &str, convoy: &str, status: TicketStatus) {
        TicketRepo::upsert(
            db,
            &Ticket {
                key: TicketKey(key.into()),
                convoy: Some(ConvoyKey(convoy.into())),
                held: false,
                created_at: "t".into(),
                blocked_by: vec![],
                title: None,
                assignee: None,
                status: Some(status),
                synced_at: None,
            },
        )
        .unwrap();
    }

    fn order_wt(db: &Ledger, key: &str, auto_impl: bool, worktree: Option<&str>) {
        OrderRepo::dispatch(
            db,
            &Order {
                ticket: TicketKey(key.into()),
                auto_impl,
                fork_from: "origin/master".into(),
                ac_summary: "do it".into(),
                park_at: None,
                worktree_path: worktree.map(Into::into),
                created_at: "t".into(),
            },
        )
        .unwrap();
    }

    fn session(db: &Ledger, kind: CrewKind, key: &str, ended: bool) {
        let id = SessionRepo::start(
            db,
            &Session {
                id: SessionId(0),
                kind,
                ticket: Some(TicketKey(key.into())),
                convoy: None,
                worktree_path: "/wt".into(),
                tmux_session: format!("{}-{key}", kind.as_str()),
                harness_session_id: None,
                created_at: "t".into(),
                ended_at: None,
                anchored_at: None,
            },
        )
        .unwrap()
        .expect("session start");
        if ended {
            SessionRepo::end(db, &id, "t2").unwrap();
        }
    }

    fn ship(db: &Ledger, iid: i64, key: &str, state: ShipState) {
        let id = ShipRepo::upsert(
            db,
            &Ship {
                id: ShipId(0),
                ticket: Some(TicketKey(key.into())),
                iid,
                url: "u".into(),
                target: "master".into(),
                source: String::new(),
                pilot: "p".into(),
                created_at: "t".into(),
                ended_at: None,
                swept: None,
                reviewed: None,
            },
        )
        .unwrap();
        db.sync_snapshot(&ShipSnapshot {
            ship: id,
            title: "T".into(),
            state,
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
    }

    /// Mark a ship reviewed at its current head — the lookout's change-marker (an ally that's been
    /// reviewed at head isn't re-reviewed until its code moves).
    fn mark_reviewed(db: &Ledger, iid: i64) {
        let s = db.armada().unwrap().into_iter().find(|s| s.iid == iid).unwrap();
        let head = ShipRepo::snapshot(db, &s.id).unwrap().unwrap().head_sha;
        db.set_reviewed(&s.id, &head).unwrap();
    }

    /// An ALLIED ship (an MR we don't own — no order; here no ticket either) + its snapshot, for the lookout tests.
    fn allied_ship(db: &Ledger, iid: i64, state: ShipState) {
        let id = ShipRepo::upsert(
            db,
            &Ship {
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
            },
        )
        .unwrap();
        db.sync_snapshot(&ShipSnapshot {
            ship: id,
            title: "T".into(),
            state,
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
    }

    /// A live, ticketless singleton lookout cog (not started in the ledger — plan(, true) reads it from
    /// the `cogs` slice).
    fn lookout_cog() -> Cog {
        Cog {
            session: Session {
                id: SessionId(1),
                kind: CrewKind::Lookout,
                ticket: None,
                convoy: None,
                worktree_path: "/project".into(),
                tmux_session: "lookout".into(),
                harness_session_id: None,
                created_at: "t".into(),
                ended_at: None,
                anchored_at: None,
            },
            liveness: Liveness::Active,
        }
    }

    fn dead_cog(key: &str, kind: CrewKind) -> Cog {
        Cog {
            session: Session {
                id: SessionId(1),
                kind,
                ticket: Some(TicketKey(key.into())),
                convoy: None,
                worktree_path: "/wt".into(),
                tmux_session: format!("{}-{key}", kind.as_str()),
                harness_session_id: None,
                created_at: "t".into(),
                ended_at: None,
                anchored_at: None,
            },
            liveness: Liveness::Dead,
        }
    }

    /// A live, ticketless captain cog on `convoy` (not started in the ledger — plan(, true) reads
    /// captains from the `cogs` slice).
    fn captain_cog(convoy: &str) -> Cog {
        Cog {
            session: Session {
                id: SessionId(1),
                kind: CrewKind::Captain,
                ticket: None,
                convoy: Some(ConvoyKey(convoy.into())),
                worktree_path: "/project".into(),
                tmux_session: format!("captain-{convoy}"),
                harness_session_id: None,
                created_at: "t".into(),
                ended_at: None,
                anchored_at: None,
            },
            liveness: Liveness::Active,
        }
    }

    /// Start a REAL ledger session (so `latest()` can join it) and return it as a Cog
    /// with the chosen liveness.
    fn live_cog(db: &Ledger, kind: CrewKind, key: &str, liveness: Liveness) -> Cog {
        let id = SessionRepo::start(
            db,
            &Session {
                id: SessionId(0),
                kind,
                ticket: Some(TicketKey(key.into())),
                convoy: None,
                worktree_path: "/wt".into(),
                tmux_session: format!("{}-{key}", kind.as_str()),
                harness_session_id: None,
                created_at: "t".into(),
                ended_at: None,
                anchored_at: None,
            },
        )
        .unwrap()
        .expect("session start");
        Cog { session: SessionRepo::get(db, &id).unwrap().unwrap(), liveness }
    }

    fn report(db: &Ledger, cog: &Cog, state: CogState) {
        ReportRepo::record(
            db,
            &Report { session: cog.session.id.clone(), state, last_action: None, reported_at: "t".into() },
        )
        .unwrap();
    }

    // ── spawn ───────────────────────────────────────────────────────────────────────

    #[test]
    fn spawns_a_ready_dispatched_ticket() {
        let db = db();
        ticket(&db, "PROJ-1", TicketStatus::Todo, false, vec![]);
        order(&db, "PROJ-1", true);
        assert_eq!(
            plan(&db, &[], true).unwrap(),
            vec![Action::Spawn { ticket: "PROJ-1".into(), kind: "worker".into() }]
        );
    }

    #[test]
    fn spawns_a_hand_when_auto_impl_false() {
        let db = db();
        ticket(&db, "PROJ-1", TicketStatus::Todo, false, vec![]);
        order(&db, "PROJ-1", false);
        assert_eq!(plan(&db, &[], true).unwrap()[0], Action::Spawn { ticket: "PROJ-1".into(), kind: "hand".into() });
    }

    #[test]
    fn spawns_a_hand_on_a_non_todo_ticket() {
        // A hand is operator intent, not frontier work — it spawns wherever the operator
        // dispatched it, regardless of status (a worker here would gate on is_ready → no spawn).
        let db = db();
        ticket(&db, "PROJ-1", TicketStatus::InProgress, false, vec![]);
        order(&db, "PROJ-1", false); // hand order
        assert_eq!(plan(&db, &[], true).unwrap()[0], Action::Spawn { ticket: "PROJ-1".into(), kind: "hand".into() });
    }

    #[test]
    fn no_spawn_without_an_order() {
        let db = db();
        ticket(&db, "PROJ-1", TicketStatus::Todo, false, vec![]);
        assert!(plan(&db, &[], true).unwrap().is_empty());
    }

    #[test]
    fn no_spawn_when_a_session_already_exists() {
        let db = db();
        ticket(&db, "PROJ-1", TicketStatus::Todo, false, vec![]);
        order(&db, "PROJ-1", true);
        session(&db, CrewKind::Worker, "PROJ-1", false);
        assert!(plan(&db, &[], true).unwrap().is_empty());
    }

    #[test]
    fn no_spawn_when_held_or_not_todo() {
        let db = db();
        ticket(&db, "PROJ-1", TicketStatus::Todo, true, vec![]);
        order(&db, "PROJ-1", true);
        ticket(&db, "PROJ-2", TicketStatus::InProgress, false, vec![]);
        order(&db, "PROJ-2", true);
        assert!(plan(&db, &[], true).unwrap().is_empty());
    }

    #[test]
    fn blocked_until_the_blocker_is_terminal() {
        let db = db();
        ticket(&db, "PROJ-2", TicketStatus::InProgress, false, vec![]);
        ticket(&db, "PROJ-1", TicketStatus::Todo, false, vec!["PROJ-2"]);
        order(&db, "PROJ-1", true);
        assert!(plan(&db, &[], true).unwrap().is_empty(), "blocked while PROJ-2 is in progress");
        ticket(&db, "PROJ-2", TicketStatus::Done, false, vec![]);
        assert_eq!(plan(&db, &[], true).unwrap().len(), 1);
    }

    #[test]
    fn spawns_a_stacked_ticket_off_its_in_flight_parent() {
        // Child blocked by an IN-FLIGHT parent (open ship to fork from), one level deep → STACKED,
        // so it spawns early off the parent's branch instead of waiting for the parent to merge.
        let db = db();
        ticket(&db, "PROJ-1", TicketStatus::InQa, false, vec![]); // parent, in-flight
        ship(&db, 101, "PROJ-1", ShipState::Open); // its open MR — the fork base
        ticket(&db, "PROJ-2", TicketStatus::Todo, false, vec!["PROJ-1"]); // child blocked_by parent
        order(&db, "PROJ-2", true);
        assert!(
            plan(&db, &[], true).unwrap().contains(&Action::Spawn { ticket: "PROJ-2".into(), kind: "worker".into() }),
            "a dispatched STACKED child spawns off its in-flight parent"
        );
    }

    #[test]
    fn a_forge_that_cant_stack_never_stacks() {
        // GitHub has no MR dependencies: a child of an in-flight parent is BLOCKED (it waits for
        // the merge, then dispatches off the default branch).
        let db = db();
        ticket(&db, "PROJ-1", TicketStatus::InQa, false, vec![]);
        ship(&db, 101, "PROJ-1", ShipState::Open);
        ticket(&db, "PROJ-2", TicketStatus::Todo, false, vec!["PROJ-1"]);
        order(&db, "PROJ-2", true);
        let t2 = TicketRepo::get(&db, &TicketKey("PROJ-2".into())).unwrap().unwrap();
        assert_eq!(classify(&db, &t2, &db.armada().unwrap(), false).unwrap(), Some(Classification::Blocked));
        assert!(!plan(&db, &[], false).unwrap().iter().any(|a| matches!(a, Action::Spawn { .. })));
    }

    #[test]
    fn no_spawn_when_blocker_has_no_ship_to_fork_from() {
        // Same shape but the parent has NO open ship → BLOCKED, not STACKED → the child waits.
        let db = db();
        ticket(&db, "PROJ-1", TicketStatus::InProgress, false, vec![]); // parent, no ship
        ticket(&db, "PROJ-2", TicketStatus::Todo, false, vec!["PROJ-1"]);
        order(&db, "PROJ-2", true);
        assert!(
            !plan(&db, &[], true).unwrap().iter().any(|a| matches!(a, Action::Spawn { .. })),
            "a blocked child with no parent ship to fork from doesn't spawn"
        );
    }

    // ── handoff ───────────────────────────────────────────────────────────────────

    #[test]
    fn handoff_when_builder_reports_concluded() {
        let db = db();
        ticket(&db, "PROJ-1", TicketStatus::InProgress, false, vec![]);
        let w = live_cog(&db, CrewKind::Worker, "PROJ-1", Liveness::Quiet);
        report(&db, &w, CogState::Worker(WorkerState::Concluded));
        assert_eq!(plan(&db, &[w], true).unwrap(), vec![Action::Handoff { ticket: "PROJ-1".into() }]);
    }

    #[test]
    fn no_handoff_before_the_builder_concludes() {
        let db = db();
        ticket(&db, "PROJ-1", TicketStatus::InProgress, false, vec![]);
        let w = live_cog(&db, CrewKind::Worker, "PROJ-1", Liveness::Active);
        report(&db, &w, CogState::Worker(WorkerState::Implementing)); // not done yet
        assert!(plan(&db, &[w], true).unwrap().is_empty());
    }

    #[test]
    fn no_handoff_while_a_sailor_is_live() {
        let db = db();
        ticket(&db, "PROJ-1", TicketStatus::InQa, false, vec![]);
        let w = live_cog(&db, CrewKind::Worker, "PROJ-1", Liveness::Quiet);
        report(&db, &w, CogState::Worker(WorkerState::Concluded));
        session(&db, CrewKind::Sailor, "PROJ-1", false); // sailor already sailing
        assert!(plan(&db, &[w], true).unwrap().is_empty());
    }

    #[test]
    fn concluded_builder_hands_off_even_when_dead() {
        // Reported `concluded` then its tmux died — that's a finish, not a crash.
        let db = db();
        ticket(&db, "PROJ-1", TicketStatus::InProgress, false, vec![]);
        let w = live_cog(&db, CrewKind::Worker, "PROJ-1", Liveness::Dead);
        report(&db, &w, CogState::Worker(WorkerState::Concluded));
        assert_eq!(plan(&db, &[w], true).unwrap(), vec![Action::Handoff { ticket: "PROJ-1".into() }]);
    }

    // ── reap ──────────────────────────────────────────────────────────────────────

    #[test]
    fn reap_when_ship_merged_and_a_session_is_live() {
        let db = db();
        ticket(&db, "PROJ-1", TicketStatus::InQa, false, vec![]);
        session(&db, CrewKind::Sailor, "PROJ-1", false); // live sailor to reap
        ship(&db, 10, "PROJ-1", ShipState::Merged);
        assert_eq!(plan(&db, &[], true).unwrap(), vec![Action::Reap { ticket: "PROJ-1".into() }]);
    }

    #[test]
    fn a_terminal_ticket_with_an_order_reaps_with_no_cog_left() {
        let db = db();
        ticket(&db, "PROJ-1", TicketStatus::Cancelled, false, vec![]);
        order(&db, "PROJ-1", true); // no session at all: nothing live to trigger the per-cog reap
        assert_eq!(plan(&db, &[], true).unwrap(), vec![Action::Reap { ticket: "PROJ-1".into() }]);
    }

    #[test]
    fn terminal_ticket_reaps_its_live_session() {
        let db = db();
        ticket(&db, "PROJ-1", TicketStatus::Done, false, vec![]);
        let cogs = vec![dead_cog("PROJ-1", CrewKind::Sailor)]; // live-in-ledger, ticket Done
        assert_eq!(plan(&db, &cogs, true).unwrap(), vec![Action::Reap { ticket: "PROJ-1".into() }]);
    }

    // ── heal ──────────────────────────────────────────────────────────────────────

    #[test]
    fn heal_relaunches_its_own_id_fresh_when_no_transcript_exists() {
        // `/wt` has no transcript, so claude must start the id fresh (`--resume` would refuse).
        let mut cog = dead_cog("PROJ-1", CrewKind::Hand);
        assert_eq!(Launch::relaunch(&cog.session), Launch::Plain, "no stored id");
        cog.session.harness_session_id = Some("abc-123".into());
        assert_eq!(Launch::relaunch(&cog.session), Launch::Fresh("abc-123"));
    }

    #[test]
    fn launch_cmd_resumes_or_starts_fresh() {
        assert_eq!(launch_cmd("hand", Launch::Resume("abc")), "claude --resume abc --agent hand");
        assert_eq!(launch_cmd("hand", Launch::Fresh("abc")), "claude --session-id abc --agent hand");
        assert_eq!(launch_cmd("hand", Launch::Plain), "claude --agent hand");
    }

    #[test]
    fn heal_a_dead_live_session_on_an_active_ticket() {
        let db = db();
        ticket(&db, "PROJ-1", TicketStatus::InProgress, false, vec![]);
        let cogs = vec![dead_cog("PROJ-1", CrewKind::Worker)];
        assert_eq!(
            plan(&db, &cogs, true).unwrap(),
            vec![Action::Heal { ticket: "PROJ-1".into(), kind: "worker".into() }]
        );
    }

    #[test]
    fn no_heal_when_the_session_is_anchored() {
        // Dead + no deliberate-stop report would normally heal; the operator anchor holds it
        // dormant (the authoritative hold, even before the cog self-reports `anchored`).
        let db = db();
        ticket(&db, "PROJ-1", TicketStatus::InProgress, false, vec![]);
        let mut cog = dead_cog("PROJ-1", CrewKind::Worker);
        cog.session.anchored_at = Some("t1".into());
        assert!(plan(&db, &[cog], true).unwrap().is_empty());
    }

    #[test]
    fn no_heal_when_the_cog_reported_a_deliberate_pause() {
        // Escalated → waiting on an operator drain, not a crash: leave it alone.
        let db = db();
        ticket(&db, "PROJ-1", TicketStatus::InProgress, false, vec![]);
        let w = live_cog(&db, CrewKind::Worker, "PROJ-1", Liveness::Dead);
        report(&db, &w, CogState::Worker(WorkerState::Escalated));
        assert!(plan(&db, &[w], true).unwrap().is_empty());
    }

    // ── dry-run ───────────────────────────────────────────────────────────────────

    #[test]
    fn dry_run_tick_logs_the_plan_and_executes_nothing() {
        let db = db();
        ticket(&db, "PROJ-1", TicketStatus::Todo, false, vec![]);
        order(&db, "PROJ-1", true);

        let tracker = FakeTracker::new();
        let host = FakeHost::new();
        let n = tick(&db, &tracker, &host, &FakeForge::new(), &[], "/project", true).unwrap();
        assert_eq!(n, 1);

        let ev = db.since(Seq(0)).unwrap();
        let t = ev.iter().find(|e| e.entity_ref == "reconcile").expect("a reconcile event");
        assert_eq!(t.kind, EventKind::TickRan);
        assert_eq!(t.payload["dry_run"], true);
        assert_eq!(t.payload["planned"][0]["action"], "spawn");
        assert_eq!(t.payload["planned"][0]["ticket"], "PROJ-1");

        // dry-run executed nothing: no session, no Jira claim, no launch.
        assert!(SessionRepo::for_ticket(&db, &TicketKey("PROJ-1".into())).unwrap().is_empty());
        assert!(tracker.moves.borrow().is_empty());
        assert!(host.spawned.borrow().is_empty());
    }

    // ── executor: spawn (live, through the ports) ────────────────────────────────────

    /// Records every Jira transition asked of it; `get`/`active_sprint` are unused by the
    /// executor, so they're left unimplemented.
    struct FakeTracker {
        moves: RefCell<Vec<(String, TicketStatus)>>,
        claims: RefCell<Vec<String>>, // tickets claimed
        peer: RefCell<bool>,          // what assigned_to_peer returns
        fail_claim: RefCell<bool>,    // force the claim to fail (best-effort test)
        parent: RefCell<Option<TicketKey>>, // what parent_of returns
        subtasks: RefCell<Vec<TicketStatus>>, // what subtasks returns
    }
    impl FakeTracker {
        fn new() -> Self {
            Self {
                moves: RefCell::new(vec![]),
                claims: RefCell::new(vec![]),
                peer: RefCell::new(false),
                fail_claim: RefCell::new(false),
                parent: RefCell::new(None),
                subtasks: RefCell::new(vec![]),
            }
        }
    }
    impl Tracker for FakeTracker {
        fn preflight(&self) -> Result<String> {
            Ok("fake".into())
        }
        fn board(&self) -> Result<Vec<TrackerTicket>> {
            Ok(vec![])
        }
        fn get(&self, _key: &TicketKey) -> Result<TrackerTicket> {
            unimplemented!("the executor never reads the tracker")
        }
        fn transition(&self, key: &TicketKey, to: TicketStatus) -> Result<()> {
            self.moves.borrow_mut().push((key.0.clone(), to));
            Ok(())
        }
        fn assigned_to_peer(&self, _key: &TicketKey) -> Result<bool> {
            Ok(*self.peer.borrow())
        }
        fn claim(&self, key: &TicketKey) -> Result<()> {
            if *self.fail_claim.borrow() {
                return Err(anyhow!("forced partial claim"));
            }
            self.claims.borrow_mut().push(key.0.clone());
            Ok(())
        }
        fn parent_of(&self, _key: &TicketKey) -> Result<Option<TicketKey>> {
            Ok(self.parent.borrow().clone())
        }
        fn subtasks(&self, _parent: &TicketKey) -> Result<Vec<TicketStatus>> {
            Ok(self.subtasks.borrow().clone())
        }
        fn read(&self, _: &TicketKey) -> Result<domain::TicketDetail> {
            unimplemented!()
        }
        fn create(&self, _: &domain::TicketDraft) -> Result<TicketKey> {
            unimplemented!()
        }
        fn edit(&self, _: &TicketKey, _: &str) -> Result<()> {
            Ok(())
        }
        fn label(&self, _: &TicketKey, _: &str, _: bool) -> Result<()> {
            unimplemented!()
        }
        fn block(&self, _: &TicketKey, _: &TicketKey) -> Result<()> {
            Ok(())
        }
        fn comment(&self, _: &TicketKey, _: &str) -> Result<()> {
            Ok(())
        }
    }

    /// `snapshot` returns a configured `ForgeShip` (for LaunchSailor's fresh poll) or errors,
    /// in which case `launch_sailor` falls back to the last synced snapshot.
    struct FakeForge {
        snapshot: RefCell<Option<ForgeShip>>,
        review_requested: RefCell<Vec<String>>, // tickets request_review was called for
        blocks: RefCell<Vec<(i64, Vec<i64>)>>,  // the sync_blocks calls
    }
    impl FakeForge {
        fn new() -> Self {
            Self { snapshot: RefCell::new(None), review_requested: RefCell::new(vec![]), blocks: RefCell::new(vec![]) }
        }
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
        fn snapshot(&self, _iid: i64) -> Result<ForgeShip> {
            self.snapshot.borrow().clone().ok_or_else(|| anyhow!("no fake snapshot"))
        }
        fn review_state(&self, _iid: i64) -> Result<ReviewState> {
            unimplemented!()
        }
        fn rebase(&self, _iid: i64) -> Result<()> {
            Ok(())
        }
        fn sync_blocks(&self, child: i64, parents: &[i64]) -> Result<usize> {
            self.blocks.borrow_mut().push((child, parents.to_vec()));
            Ok(0)
        }
        fn arm_merge(&self, _iid: i64) -> Result<()> {
            Ok(())
        }
        fn disarm(&self, _iid: i64) -> Result<()> {
            Ok(())
        }
        fn close_mr(&self, _: i64) -> Result<()> {
            Ok(())
        }
        fn set_draft(&self, _: i64, _: bool) -> Result<()> {
            Ok(())
        }
        fn open_mr(&self, _: &domain::MrDraft) -> Result<String> {
            Ok("https://x/1".into())
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
        fn post_review(&self, _: i64, _: &str, _: bool) -> Result<()> {
            Ok(())
        }
        fn request_review(&self, ticket: &TicketKey) -> Result<Vec<String>> {
            self.review_requested.borrow_mut().push(ticket.0.clone());
            Ok(vec!["alice".into(), "bob".into()])
        }
    }

    /// Records spawns; the liveness probes return harmless defaults.
    struct FakeHost {
        spawned: RefCell<Vec<(String, String, String)>>, // (name, cwd, cmd)
        killed: RefCell<Vec<String>>,
    }
    impl FakeHost {
        fn new() -> Self {
            Self { spawned: RefCell::new(vec![]), killed: RefCell::new(vec![]) }
        }
    }
    impl SessionHost for FakeHost {
        fn preflight(&self) -> Result<String> {
            Ok("fake".into())
        }
        fn spawn(&self, name: &str, cwd: &Path, cmd: &str) -> Result<()> {
            self.spawned.borrow_mut().push((name.into(), cwd.display().to_string(), cmd.into()));
            Ok(())
        }
        fn alive(&self, _name: &str) -> Result<bool> {
            Ok(false)
        }
        fn last_activity(&self, _name: &str) -> Result<Option<i64>> {
            Ok(None)
        }
        fn kill(&self, name: &str) -> Result<()> {
            self.killed.borrow_mut().push(name.into());
            Ok(())
        }
        fn send(&self, _name: &str, _text: &str) -> Result<()> {
            Ok(())
        }
        fn list(&self) -> Result<Vec<String>> {
            Ok(vec![])
        }
    }

    #[test]
    fn executes_spawn_claims_jira_records_the_session_and_launches() {
        let db = db();
        ticket(&db, "PROJ-1", TicketStatus::Todo, false, vec![]);
        order_wt(&db, "PROJ-1", true, Some("/wt/PROJ-1"));

        let tracker = FakeTracker::new();
        let host = FakeHost::new();
        assert_eq!(tick(&db, &tracker, &host, &FakeForge::new(), &[], "/project", false).unwrap(), 1);

        // Jira claimed: To Do → In Progress.
        assert_eq!(
            tracker.moves.borrow().as_slice(),
            &[("PROJ-1".to_string(), TicketStatus::InProgress)]
        );
        // ...then the claim follow-ons.
        assert_eq!(tracker.claims.borrow().as_slice(), &["PROJ-1".to_string()]);
        // a live worker session now exists in the order's worktree.
        let sessions = SessionRepo::for_ticket(&db, &TicketKey("PROJ-1".into())).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].kind, CrewKind::Worker);
        assert_eq!(sessions[0].worktree_path, "/wt/PROJ-1");
        assert!(sessions[0].ended_at.is_none());
        // fleetd minted + captured a session id for the transcript (the #22 capture).
        assert!(sessions[0].harness_session_id.is_some(), "a session id was captured at spawn");
        // the cog was launched with that id: claude --session-id <uuid> --agent worker.
        let spawned = host.spawned.borrow();
        assert_eq!(spawned.len(), 1);
        let (name, cwd, cmd) = &spawned[0];
        assert_eq!(name, "worker-PROJ-1");
        assert_eq!(cwd, "/wt/PROJ-1");
        assert!(
            cmd.starts_with("FLEET_TICKET=PROJ-1 claude --session-id ") && cmd.ends_with(" --agent worker"),
            "cmd was: {cmd}"
        );
        // a SessionStarted CDC event was emitted for the ticket.
        let ev = db.since(Seq(0)).unwrap();
        assert!(ev
            .iter()
            .any(|e| e.kind == EventKind::SessionStarted && e.payload["ticket"] == "PROJ-1"));
    }

    #[test]
    fn executes_hand_spawn_skips_the_claim_ceremony_but_records_and_launches() {
        // A hand is operator intent — it must not move the ticket's lane, re-label, or re-sprint.
        // So a hand spawn touches Jira for NOTHING (not even the transition); it just records the
        // session + launches in the worktree.
        let db = db();
        ticket(&db, "PROJ-1", TicketStatus::InProgress, false, vec![]);
        order_wt(&db, "PROJ-1", false, Some("/wt/PROJ-1")); // hand order

        let tracker = FakeTracker::new();
        let host = FakeHost::new();
        assert_eq!(tick(&db, &tracker, &host, &FakeForge::new(), &[], "/project", false).unwrap(), 1);

        assert!(tracker.moves.borrow().is_empty(), "a hand never transitions the ticket");
        assert!(tracker.claims.borrow().is_empty(), "a hand runs no claim follow-ons");
        // ...but the session is recorded and the cog launched.
        let sessions = SessionRepo::for_ticket(&db, &TicketKey("PROJ-1".into())).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].kind, CrewKind::Hand);
        assert!(sessions[0].ended_at.is_none());
        assert_eq!(host.spawned.borrow().first().map(|s| s.0.as_str()), Some("hand-PROJ-1"));
    }

    #[test]
    fn spawns_a_ticketless_hand_in_a_worktree() {
        // `fleetc hand` path: a hand with NO ticket + NO order, recorded as a tracked session and
        // launched as hand-<worktree-basename>.
        let db = db();
        let host = FakeHost::new();
        spawn_ticketless_hand(&db, &host, "/wt/scratch").unwrap();
        let live = SessionRepo::live(&db).unwrap();
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].kind, CrewKind::Hand);
        assert!(live[0].ticket.is_none(), "ticketless");
        assert_eq!(live[0].worktree_path, "/wt/scratch");
        let spawned = host.spawned.borrow();
        assert_eq!(spawned[0].0, "hand-scratch");
        assert!(spawned[0].2.ends_with("--agent hand"), "cmd: {}", spawned[0].2);
    }

    #[test]
    fn spawn_without_a_worktree_is_recorded_as_failed_and_never_claims_jira() {
        let db = db();
        ticket(&db, "PROJ-1", TicketStatus::Todo, false, vec![]);
        order_wt(&db, "PROJ-1", true, None); // producer hasn't materialised a worktree

        let tracker = FakeTracker::new();
        let host = FakeHost::new();
        assert_eq!(tick(&db, &tracker, &host, &FakeForge::new(), &[], "/project", false).unwrap(), 0, "nothing applied");

        assert!(tracker.moves.borrow().is_empty(), "no Jira claim without a worktree");
        assert!(SessionRepo::for_ticket(&db, &TicketKey("PROJ-1".into())).unwrap().is_empty());
        assert!(host.spawned.borrow().is_empty());
        // the failure is recorded on the live TickRan event (never fatal).
        let ev = db.since(Seq(0)).unwrap();
        let tick_ev =
            ev.iter().rev().find(|e| e.entity_ref == "reconcile").expect("a TickRan event");
        assert_eq!(tick_ev.payload["dry_run"], false);
        assert_eq!(tick_ev.payload["failed"][0]["action"]["action"], "spawn");
    }

    #[test]
    fn live_tick_with_no_actions_executes_nothing() {
        let db = db();
        let tracker = FakeTracker::new();
        let host = FakeHost::new();
        assert_eq!(tick(&db, &tracker, &host, &FakeForge::new(), &[], "/project", false).unwrap(), 0);
        assert!(tracker.moves.borrow().is_empty());
        assert!(host.spawned.borrow().is_empty());
    }

    #[test]
    fn spawn_is_a_no_op_when_a_live_cog_already_holds_the_lock() {
        let db = db();
        ticket(&db, "PROJ-1", TicketStatus::Todo, false, vec![]);
        order_wt(&db, "PROJ-1", true, Some("/wt"));
        session(&db, CrewKind::Worker, "PROJ-1", false); // a live worker already holds the slot

        let tracker = FakeTracker::new();
        let host = FakeHost::new();
        // Call the executor path directly: plan(, true) would gate this out, but the DB lock is the
        // real guard — a lock hit must be a benign skip, not a failure.
        spawn(&db, &tracker, &host, "PROJ-1", "worker").unwrap();

        // no duplicate session, no second launch.
        assert_eq!(SessionRepo::for_ticket(&db, &TicketKey("PROJ-1".into())).unwrap().len(), 1);
        assert!(host.spawned.borrow().is_empty(), "no launch when the lock skips the spawn");
    }

    #[test]
    fn spawn_skips_a_ticket_owned_by_a_peer() {
        let db = db();
        ticket(&db, "PROJ-1", TicketStatus::Todo, false, vec![]);
        order_wt(&db, "PROJ-1", true, Some("/wt/PROJ-1"));
        let tracker = FakeTracker::new();
        *tracker.peer.borrow_mut() = true; // a teammate owns it
        let host = FakeHost::new();

        spawn(&db, &tracker, &host, "PROJ-1", "worker").unwrap();

        // the peer is respected: nothing moved, nothing claimed, no session, no launch.
        assert!(tracker.moves.borrow().is_empty(), "no transition on a peer's ticket");
        assert!(tracker.claims.borrow().is_empty());
        assert!(SessionRepo::for_ticket(&db, &TicketKey("PROJ-1".into())).unwrap().is_empty());
        assert!(host.spawned.borrow().is_empty());
    }

    #[test]
    fn spawn_survives_a_partial_claim() {
        // A claim follow-on failing is a partial claim — logged, not fatal: the ticket is still
        // transitioned, the session recorded, the cog launched.
        let db = db();
        ticket(&db, "PROJ-1", TicketStatus::Todo, false, vec![]);
        order_wt(&db, "PROJ-1", true, Some("/wt/PROJ-1"));
        let tracker = FakeTracker::new();
        *tracker.fail_claim.borrow_mut() = true;
        let host = FakeHost::new();

        spawn(&db, &tracker, &host, "PROJ-1", "worker").unwrap();

        assert_eq!(tracker.moves.borrow().len(), 1, "transitioned despite the partial claim");
        assert_eq!(SessionRepo::for_ticket(&db, &TicketKey("PROJ-1".into())).unwrap().len(), 1);
        assert_eq!(host.spawned.borrow().len(), 1, "the cog still launched");
    }

    // ── executor: handoff ────────────────────────────────────────────────────────────

    #[test]
    fn executes_handoff_moves_to_in_qa_and_reaps_the_builder() {
        let db = db();
        ticket(&db, "PROJ-1", TicketStatus::InProgress, false, vec![]);
        let w = live_cog(&db, CrewKind::Worker, "PROJ-1", Liveness::Quiet);
        report(&db, &w, CogState::Worker(WorkerState::Concluded));

        let tracker = FakeTracker::new();
        let host = FakeHost::new();
        let forge = FakeForge::new();
        assert_eq!(tick(&db, &tracker, &host, &forge, &[w], "/project", false).unwrap(), 1);

        // Jira moved In Progress → In QA.
        assert_eq!(tracker.moves.borrow().as_slice(), &[("PROJ-1".to_string(), TicketStatus::InQa)]);
        // The ready MR was handed to the team's review (once, at handoff).
        assert_eq!(forge.review_requested.borrow().as_slice(), &["PROJ-1".to_string()]);
        // Handoff does NOT launch the sailor (LaunchSailor owns that) — it only reaps the builder.
        assert!(host.spawned.borrow().is_empty(), "handoff launches no sailor");
        assert_eq!(host.killed.borrow().as_slice(), &["worker-PROJ-1".to_string()]);
        // ledger: the builder is ended; no sailor was created here.
        let sessions = SessionRepo::for_ticket(&db, &TicketKey("PROJ-1".into())).unwrap();
        let builder = sessions.iter().find(|s| s.kind == CrewKind::Worker).unwrap();
        assert!(builder.ended_at.is_some(), "builder reaped");
        assert!(sessions.iter().all(|s| s.kind != CrewKind::Sailor), "no sailor from handoff");
        // CDC: builder SessionEnded (reason=handoff); no sailor SessionStarted.
        let ev = db.since(Seq(0)).unwrap();
        assert!(ev.iter().any(|e| e.kind == EventKind::SessionEnded && e.payload["reason"] == "handoff"));
        assert!(!ev.iter().any(|e| e.kind == EventKind::SessionStarted && e.payload["kind"] == "sailor"));
    }

    #[test]
    fn hand_handoff_promotes_its_order_to_auto_so_a_sailor_takes_over() {
        // The "pawn it off" flow: a HAND the operator drove reports concluded → handoff promotes
        // its manual order to AUTO (so LaunchSailor engages a sailor), moves Jira → In QA, reaps it.
        let db = db();
        ticket(&db, "PROJ-1", TicketStatus::InProgress, false, vec![]);
        order_wt(&db, "PROJ-1", false, Some("/wt/PROJ-1")); // manual (hand) order
        let h = live_cog(&db, CrewKind::Hand, "PROJ-1", Liveness::Quiet);
        report(&db, &h, CogState::Hand(HandState::Concluded));

        let tracker = FakeTracker::new();
        let host = FakeHost::new();
        assert_eq!(tick(&db, &tracker, &host, &FakeForge::new(), &[h], "/project", false).unwrap(), 1);

        assert!(
            OrderRepo::get(&db, &TicketKey("PROJ-1".into())).unwrap().unwrap().auto_impl,
            "the hand's order is promoted to auto → LaunchSailor will engage a sailor",
        );
        assert_eq!(tracker.moves.borrow().as_slice(), &[("PROJ-1".to_string(), TicketStatus::InQa)]);
        assert_eq!(host.killed.borrow().as_slice(), &["hand-PROJ-1".to_string()], "the hand is reaped");
    }

    // ── LaunchSailor (the cold-loop sweep trigger) ───────────────────────────────────

    #[test]
    fn launches_a_sailor_when_an_open_ship_changed_since_last_sweep() {
        let db = db();
        ticket(&db, "PROJ-1", TicketStatus::InQa, false, vec![]);
        order_wt(&db, "PROJ-1", true, Some("/wt/PROJ-1")); // an AUTO order with a worktree → sailor-eligible
        ship(&db, 10, "PROJ-1", ShipState::Open); // swept = None → first sweep always fires
        assert_eq!(plan(&db, &[], true).unwrap(), vec![Action::LaunchSailor { ticket: "PROJ-1".into() }]);
    }

    #[test]
    fn no_launch_on_a_manual_order_or_an_undispatched_ship() {
        // A manual order (a hand, operator-driven) spawns a HAND — never a cold-loop sailor —
        // even though its ship changed; the hand lands it. And an open ship with NO order (never
        // dispatched by the fleet) gets nothing — the fleet only lands work it commissioned.
        let d1 = db(); // `d1`/`d2`, not `db` — a `let db` shadows the db() helper below
        ticket(&d1, "PROJ-1", TicketStatus::InQa, false, vec![]);
        order_wt(&d1, "PROJ-1", false, Some("/wt/PROJ-1")); // auto_impl=false → hand
        ship(&d1, 10, "PROJ-1", ShipState::Open);
        assert_eq!(
            plan(&d1, &[], true).unwrap(),
            vec![Action::Spawn { ticket: "PROJ-1".into(), kind: "hand".into() }],
            "a manual/hand order spawns a hand, never a cold-loop sailor",
        );

        let d2 = db();
        ticket(&d2, "PROJ-2", TicketStatus::InQa, false, vec![]); // no order at all
        ship(&d2, 11, "PROJ-2", ShipState::Open);
        assert_eq!(plan(&d2, &[], true).unwrap(), vec![Action::LaunchLookout { iid: 11 }], "no order → no sailor; an ally's to review");
    }

    #[test]
    fn a_blocked_ship_is_swept_and_resweeps_when_its_blocker_lands() {
        // PROJ-2 is blocked by PROJ-1 (not Done): a blocker gates only the merge (`fleetc arm`
        // refuses), never the sweep — so PROJ-2's MR still gets a sailor.
        let db = db();
        ticket(&db, "PROJ-1", TicketStatus::InProgress, false, vec![]);
        ticket(&db, "PROJ-2", TicketStatus::InQa, false, vec!["PROJ-1"]);
        order_wt(&db, "PROJ-2", true, Some("/wt/PROJ-2"));
        ship(&db, 10, "PROJ-2", ShipState::Open);
        assert_eq!(plan(&db, &[], true).unwrap(), vec![Action::LaunchSailor { ticket: "PROJ-2".into() }]);

        // Swept while blocked (the sweep couldn't arm) → quiet while nothing changes…
        let s = db.armada().unwrap().into_iter().find(|s| s.iid == 10).unwrap();
        let snap = ShipRepo::snapshot(&db, &s.id).unwrap().unwrap();
        db.set_swept(&s.id, Some(&ship_fingerprint(&snap, true))).unwrap();
        assert!(plan(&db, &[], true).unwrap().is_empty(), "unchanged while PROJ-1 is open");

        // …until PROJ-1 lands: the blocker lifting is itself a change → the sweep that arms.
        ticket(&db, "PROJ-1", TicketStatus::Done, false, vec![]);
        assert_eq!(plan(&db, &[], true).unwrap(), vec![Action::LaunchSailor { ticket: "PROJ-2".into() }]);
    }

    #[test]
    fn no_launch_when_the_ship_is_unchanged_since_the_last_sweep() {
        let db = db();
        ticket(&db, "PROJ-1", TicketStatus::InQa, false, vec![]);
        order_wt(&db, "PROJ-1", true, Some("/wt/PROJ-1"));
        ship(&db, 10, "PROJ-1", ShipState::Open);
        // mark swept = the current fingerprint → nothing changed.
        let s = db.armada().unwrap().into_iter().find(|s| s.iid == 10).unwrap();
        let snap = ShipRepo::snapshot(&db, &s.id).unwrap().unwrap();
        db.set_swept(&s.id, Some(&ship_fingerprint(&snap, false))).unwrap();
        assert!(plan(&db, &[], true).unwrap().is_empty());
    }

    /// Re-sync ship `iid`'s snapshot through `mutate`, then mark swept = its (new) fingerprint —
    /// so the ship NEEDS the mutation's work but is "unchanged since last sweep" (`changed=false`).
    fn swept_with(db: &Ledger, iid: i64, mutate: impl FnOnce(&mut ShipSnapshot)) {
        let s = db.armada().unwrap().into_iter().find(|s| s.iid == iid).unwrap();
        let mut snap = ShipRepo::snapshot(db, &s.id).unwrap().unwrap();
        mutate(&mut snap);
        db.sync_snapshot(&snap).unwrap();
        let snap = ShipRepo::snapshot(db, &s.id).unwrap().unwrap();
        db.set_swept(&s.id, Some(&ship_fingerprint(&snap, false))).unwrap();
    }

    #[test]
    fn relaunches_a_sailor_on_a_failing_pipeline_even_when_unchanged() {
        // The needs-work net: a broken ship (failed pipeline) with no live sailor gets one even
        // though its fingerprint is UNCHANGED — the case fingerprint-change alone misses (an
        // escalated attendant was sunk/crashed, orphaning the failing ship).
        let db = db();
        ticket(&db, "PROJ-1", TicketStatus::InQa, false, vec![]);
        order_wt(&db, "PROJ-1", true, Some("/wt/PROJ-1"));
        ship(&db, 10, "PROJ-1", ShipState::Open);
        swept_with(&db, 10, |s| s.pipeline = Some(PipelineStatus::Failed));
        assert_eq!(
            plan(&db, &[], true).unwrap(),
            vec![Action::LaunchSailor { ticket: "PROJ-1".into() }]
        );
    }

    #[test]
    fn no_relaunch_on_a_blocked_ship_with_a_failing_pipeline() {
        // merge_request_blocked = waiting on an upstream MR, not working — needs_work excludes it
        // so we don't churn sailors on an unfixable-until-upstream failure; it re-engages via a
        // fingerprint change when the block clears.
        let db = db();
        ticket(&db, "PROJ-1", TicketStatus::InQa, false, vec![]);
        order_wt(&db, "PROJ-1", true, Some("/wt/PROJ-1"));
        ship(&db, 10, "PROJ-1", ShipState::Open);
        swept_with(&db, 10, |s| {
            s.pipeline = Some(PipelineStatus::Failed);
            s.merge_status = Some("merge_request_blocked".into());
        });
        assert!(plan(&db, &[], true).unwrap().is_empty(), "a blocked ship doesn't re-launch while unchanged");
    }

    #[test]
    fn no_relaunch_on_a_needy_ship_while_a_sailor_is_live() {
        // Loop-safety: a needy ship with a LIVE sailor (e.g. escalated, holding the slot) doesn't
        // re-launch — the live-sailor lock is what keeps needs_work from looping.
        let db = db();
        ticket(&db, "PROJ-1", TicketStatus::InQa, false, vec![]);
        order_wt(&db, "PROJ-1", true, Some("/wt/PROJ-1"));
        ship(&db, 10, "PROJ-1", ShipState::Open);
        swept_with(&db, 10, |s| s.pipeline = Some(PipelineStatus::Failed));
        session(&db, CrewKind::Sailor, "PROJ-1", false); // a live sailor holds the slot
        assert!(plan(&db, &[], true).unwrap().is_empty(), "a live sailor blocks re-launch");
    }

    #[test]
    fn no_launch_when_the_ticket_is_held() {
        let db = db();
        ticket(&db, "PROJ-1", TicketStatus::InQa, true, vec![]); // held
        order_wt(&db, "PROJ-1", true, Some("/wt/PROJ-1"));
        ship(&db, 10, "PROJ-1", ShipState::Open);
        assert!(plan(&db, &[], true).unwrap().is_empty());
    }

    #[test]
    fn no_launch_when_a_sailor_is_already_live() {
        let db = db();
        ticket(&db, "PROJ-1", TicketStatus::InQa, false, vec![]);
        order_wt(&db, "PROJ-1", true, Some("/wt/PROJ-1"));
        ship(&db, 10, "PROJ-1", ShipState::Open);
        session(&db, CrewKind::Sailor, "PROJ-1", false); // a live sailor holds it
        assert!(plan(&db, &[], true).unwrap().is_empty());
    }

    #[test]
    fn executes_launch_sailor_fresh_polls_sets_swept_and_spawns_in_the_order_worktree() {
        let db = db();
        ticket(&db, "PROJ-1", TicketStatus::InQa, false, vec![]);
        order_wt(&db, "PROJ-1", true, Some("/wt/PROJ-1")); // the sailor's worktree comes off the order
        ship(&db, 10, "PROJ-1", ShipState::Open); // helper snapshot: pipeline None

        // The fresh poll returns a DISTINCT state (pipeline Failed) so we can prove it landed.
        let forge = FakeForge::new();
        *forge.snapshot.borrow_mut() = Some(ForgeShip {
            iid: 10,
            url: "u".into(),
            target: "master".into(),
            source: String::new(),
            pilot: "me".into(),
            ticket: Some(TicketKey("PROJ-1".into())),
            title: "T".into(),
            state: ShipState::Open,
            head_sha: "fresh".into(),
            has_conflicts: false,
            open_threads: 0,
            approvals: Approvals { current: 0, required: 0 },
            armed: false,
            on_merge_train: false,
            pipeline: Some(PipelineStatus::Failed),
            merge_status: None,
        });
        let tracker = FakeTracker::new();
        let host = FakeHost::new();
        assert_eq!(tick(&db, &tracker, &host, &forge, &[], "/project", false).unwrap(), 1);

        let sessions = SessionRepo::for_ticket(&db, &TicketKey("PROJ-1".into())).unwrap();
        let sailor = sessions.iter().find(|s| s.kind == CrewKind::Sailor).expect("a sailor");
        assert!(sailor.ended_at.is_none());
        assert_eq!(sailor.worktree_path, "/wt/PROJ-1");
        let spawned = host.spawned.borrow();
        assert_eq!(spawned.len(), 1);
        let (name, cwd, cmd) = &spawned[0];
        assert_eq!((name.as_str(), cwd.as_str()), ("sailor-PROJ-1", "/wt/PROJ-1"));
        assert!(
            cmd.starts_with("FLEET_TICKET=PROJ-1 claude --session-id ") && cmd.ends_with(" --agent sailor"),
            "cmd was: {cmd}"
        );
        // the fresh poll was written to the ledger (pipeline now Failed, head_sha advanced).
        let s = db.armada().unwrap().into_iter().find(|s| s.iid == 10).unwrap();
        let snap = ShipRepo::snapshot(&db, &s.id).unwrap().unwrap();
        assert_eq!(snap.pipeline, Some(PipelineStatus::Failed), "fresh poll landed");
        assert_eq!(snap.head_sha, "fresh");
        // swept recorded (so a second tick on the same snapshot wouldn't re-launch).
        assert!(s.swept.is_some(), "swept recorded at dispatch");
    }

    // ── LaunchLookout (the singleton allied-ship review cold-loop) ──────────────────────────

    #[test]
    fn launches_the_lookout_on_an_unreviewed_allied_ship() {
        let db = db();
        allied_ship(&db, 77, ShipState::Open); // an allied MR, reviewed = None → review it
        assert_eq!(plan(&db, &[], true).unwrap(), vec![Action::LaunchLookout { iid: 77 }]);
    }

    #[test]
    fn no_lookout_launch_while_the_lookout_is_anchored() {
        // The operator anchored the review loop (its hold lives on the `lookout` row) → no relaunch,
        // even with an unreviewed ally waiting.
        let db = db();
        allied_ship(&db, 77, ShipState::Open);
        LookoutRepo::anchor(&db, "t").unwrap();
        assert!(plan(&db, &[], true).unwrap().is_empty());
    }

    #[test]
    fn the_lookout_ignores_our_own_ships_but_reviews_a_peers_ticketed_one() {
        // Ours = dispatched (an order) — the sailor's. A peer's MR names a ticket too, but no order:
        // an ally, so the lookout reviews it.
        let db = db();
        ticket(&db, "PROJ-1", TicketStatus::InQa, false, vec![]);
        ship(&db, 10, "PROJ-1", ShipState::Open);
        let lookout = |db: &Ledger| plan(db, &[], true).unwrap().into_iter().find(|a| matches!(a, Action::LaunchLookout { .. }));
        assert_eq!(lookout(&db), Some(Action::LaunchLookout { iid: 10 }), "a peer's ticketed MR is an ally");
        order(&db, "PROJ-1", true);
        assert_eq!(lookout(&db), None, "no lookout on our own ship");
    }

    #[test]
    fn no_lookout_when_every_ally_is_reviewed_at_head() {
        let db = db();
        allied_ship(&db, 77, ShipState::Open);
        mark_reviewed(&db, 77); // reviewed == head → nothing changed
        assert!(plan(&db, &[], true).unwrap().is_empty());
    }

    #[test]
    fn no_lookout_for_a_draft_ally() {
        let db = db();
        allied_ship(&db, 77, ShipState::Draft); // a draft ally isn't ready to review
        assert!(plan(&db, &[], true).unwrap().is_empty());
    }

    #[test]
    fn no_second_lookout_while_one_is_reviewing() {
        // The singleton: a live lookout blocks launching another, even on a different unreviewed ally.
        let db = db();
        allied_ship(&db, 77, ShipState::Open);
        assert!(plan(&db, &[lookout_cog()], true).unwrap().is_empty());
    }

    #[test]
    fn a_dead_lookout_stands_down() {
        let db = db();
        let mut cog = lookout_cog();
        cog.liveness = Liveness::Dead;
        assert_eq!(plan(&db, &[cog], true).unwrap(), vec![Action::StandDownLookout]);
    }

    #[test]
    fn a_live_lookout_that_reported_reviewed_stands_down_and_the_next_ally_launches() {
        // `claude --agent` doesn't self-exit: the lookout's `reviewed` report, not a dead tmux, ends
        // its pass — and frees the singleton slot for the next unreviewed ally in the same plan.
        let db = db();
        allied_ship(&db, 77, ShipState::Open);
        let id = SessionRepo::start(&db, &lookout_cog().session).unwrap().expect("session start");
        let cog = Cog { session: SessionRepo::get(&db, &id).unwrap().unwrap(), liveness: Liveness::Active };
        report(&db, &cog, CogState::Lookout(LookoutState::Escalated));
        assert!(plan(&db, std::slice::from_ref(&cog), true).unwrap().is_empty(), "an escalated lookout holds the slot");
        report(&db, &cog, CogState::Lookout(LookoutState::Reviewed));
        assert_eq!(
            plan(&db, &[cog], true).unwrap(),
            vec![Action::StandDownLookout, Action::LaunchLookout { iid: 77 }]
        );
    }

    #[test]
    fn executes_launch_lookout_marks_reviewed_and_spawns_at_the_hub_with_the_iid() {
        let db = db();
        allied_ship(&db, 77, ShipState::Open);
        let forge = FakeForge::new();
        *forge.snapshot.borrow_mut() = Some(ForgeShip {
            iid: 77,
            url: "u".into(),
            target: "master".into(),
            source: String::new(),
            pilot: "ally".into(),
            ticket: None,
            title: "T".into(),
            state: ShipState::Open,
            head_sha: "fresh".into(),
            has_conflicts: false,
            open_threads: 0,
            approvals: Approvals { current: 0, required: 0 },
            armed: false,
            on_merge_train: false,
            pipeline: None,
            merge_status: None,
        });
        let tracker = FakeTracker::new();
        let host = FakeHost::new();
        assert_eq!(tick(&db, &tracker, &host, &forge, &[], "/project", false).unwrap(), 1);

        // the singleton lookout launched in the project checkout, ticketless, with the allied iid injected.
        let live = db.live().unwrap();
        let lookout = live.iter().find(|s| s.kind == CrewKind::Lookout).expect("a lookout");
        assert!(lookout.ticket.is_none(), "the lookout is ticketless");
        assert_eq!(lookout.worktree_path, "/project");
        let spawned = host.spawned.borrow();
        assert_eq!(spawned.len(), 1);
        let (name, cwd, cmd) = &spawned[0];
        assert_eq!((name.as_str(), cwd.as_str()), ("lookout", "/project"));
        assert!(
            cmd.starts_with("FLEET_SHIP=77 claude --session-id ") && cmd.ends_with(" --agent lookout"),
            "cmd was: {cmd}"
        );
        // reviewed recorded at the fresh-polled head.
        let s = db.armada().unwrap().into_iter().find(|s| s.iid == 77).unwrap();
        assert_eq!(s.reviewed.as_deref(), Some("fresh"), "reviewed = the head we dispatched against");
    }

    #[test]
    fn executes_stand_down_lookout_ends_the_singleton_session() {
        let db = db();
        let id = SessionRepo::start(
            &db,
            &Session {
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
            },
        )
        .unwrap()
        .expect("lookout session");
        let cog = Cog { session: SessionRepo::get(&db, &id).unwrap().unwrap(), liveness: Liveness::Dead };
        let tracker = FakeTracker::new();
        let host = FakeHost::new();
        assert_eq!(tick(&db, &tracker, &host, &FakeForge::new(), &[cog], "/project", false).unwrap(), 1);
        assert!(db.live().unwrap().is_empty(), "the lookout session ended");
        assert!(host.killed.borrow().iter().any(|n| n == "lookout"));
    }

    // ── executor: reap ───────────────────────────────────────────────────────────────

    #[test]
    fn executes_reap_on_a_merged_ship_moves_to_done_reaps_the_sailor_and_retires_the_order() {
        let db = db();
        ticket(&db, "PROJ-1", TicketStatus::InQa, false, vec![]);
        order_wt(&db, "PROJ-1", true, Some("/wt")); // the fulfilled order to retire
        session(&db, CrewKind::Sailor, "PROJ-1", false); // live sailor
        ship(&db, 10, "PROJ-1", ShipState::Merged);

        let tracker = FakeTracker::new();
        let host = FakeHost::new();
        assert_eq!(tick(&db, &tracker, &host, &FakeForge::new(), &[], "/project", false).unwrap(), 1);

        // merged ship on an In QA ticket → Jira driven to Done.
        assert_eq!(tracker.moves.borrow().as_slice(), &[("PROJ-1".to_string(), TicketStatus::Done)]);
        // the sailor was reaped: session ended, tmux killed.
        let sessions = SessionRepo::for_ticket(&db, &TicketKey("PROJ-1".into())).unwrap();
        assert!(sessions.iter().all(|s| s.ended_at.is_some()), "all sessions ended");
        assert_eq!(host.killed.borrow().as_slice(), &["sailor-PROJ-1".to_string()]);
        // the order (dispatch intent) was retired.
        assert!(OrderRepo::get(&db, &TicketKey("PROJ-1".into())).unwrap().is_none());
        // CDC SessionEnded(reason=reap).
        let ev = db.since(Seq(0)).unwrap();
        assert!(ev.iter().any(|e| e.kind == EventKind::SessionEnded && e.payload["reason"] == "reap"));
    }

    #[test]
    fn executes_reap_on_a_terminal_ticket_skips_jira_and_reaps_the_session() {
        let db = db();
        ticket(&db, "PROJ-1", TicketStatus::Done, false, vec![]); // already terminal
        let c = live_cog(&db, CrewKind::Sailor, "PROJ-1", Liveness::Dead);

        let tracker = FakeTracker::new();
        let host = FakeHost::new();
        assert_eq!(tick(&db, &tracker, &host, &FakeForge::new(), &[c], "/project", false).unwrap(), 1);

        // already Done → no Jira transition.
        assert!(tracker.moves.borrow().is_empty(), "no transition on an already-terminal ticket");
        // the live session was reaped anyway.
        let sessions = SessionRepo::for_ticket(&db, &TicketKey("PROJ-1".into())).unwrap();
        assert!(sessions.iter().all(|s| s.ended_at.is_some()));
        assert_eq!(host.killed.borrow().as_slice(), &["sailor-PROJ-1".to_string()]);
    }

    // ── executor: heal ───────────────────────────────────────────────────────────────

    #[test]
    fn executes_heal_relaunches_the_dead_session_in_place() {
        let db = db();
        ticket(&db, "PROJ-1", TicketStatus::InProgress, false, vec![]);
        let w = live_cog(&db, CrewKind::Worker, "PROJ-1", Liveness::Dead); // crashed: tmux gone

        let tracker = FakeTracker::new();
        let host = FakeHost::new();
        assert_eq!(tick(&db, &tracker, &host, &FakeForge::new(), &[w], "/project", false).unwrap(), 1);

        // the tmux was relaunched in place (same session name, same worktree).
        assert_eq!(
            host.spawned.borrow().as_slice(),
            &[("worker-PROJ-1".to_string(), "/wt".to_string(), "FLEET_TICKET=PROJ-1 claude --agent worker".to_string())]
        );
        // the session row stays LIVE — the cog resumes from its status drop.
        let sessions = SessionRepo::for_ticket(&db, &TicketKey("PROJ-1".into())).unwrap();
        assert_eq!(sessions.len(), 1, "no new session row");
        assert!(sessions[0].ended_at.is_none(), "heal keeps the session live");
        // no Jira transition, nothing reaped.
        assert!(tracker.moves.borrow().is_empty());
        assert!(host.killed.borrow().is_empty());
    }

    // ── classify (fleetd's dispatch verdict, the absorbed classify.py) ───────────────

    #[test]
    fn classify_covers_clean_hold_blocked_stacked_and_off_frontier() {
        let db = db();
        let armada = |db: &Ledger| db.armada().unwrap();

        // CLEAN — To Do, unblocked, not held.
        ticket(&db, "PROJ-1", TicketStatus::Todo, false, vec![]);
        let t1 = TicketRepo::get(&db, &TicketKey("PROJ-1".into())).unwrap().unwrap();
        assert_eq!(classify(&db, &t1, &armada(&db), true).unwrap(), Some(Classification::Clean));

        // HOLD — held takes precedence over the DAG.
        ticket(&db, "PROJ-2", TicketStatus::Todo, true, vec![]);
        let t2 = TicketRepo::get(&db, &TicketKey("PROJ-2".into())).unwrap().unwrap();
        assert_eq!(classify(&db, &t2, &armada(&db), true).unwrap(), Some(Classification::Hold));

        // None — past the frontier (not To Do).
        ticket(&db, "PROJ-3", TicketStatus::InProgress, false, vec![]);
        let t3 = TicketRepo::get(&db, &TicketKey("PROJ-3".into())).unwrap().unwrap();
        assert_eq!(classify(&db, &t3, &armada(&db), true).unwrap(), None);

        // BLOCKED — a non-terminal parent with NO ship to fork from.
        ticket(&db, "PROJ-10", TicketStatus::InProgress, false, vec![]); // parent, no ship
        ticket(&db, "PROJ-11", TicketStatus::Todo, false, vec!["PROJ-10"]);
        let t11 = TicketRepo::get(&db, &TicketKey("PROJ-11".into())).unwrap().unwrap();
        assert_eq!(classify(&db, &t11, &armada(&db), true).unwrap(), Some(Classification::Blocked));

        // STACKED — the single non-terminal parent is in-flight (an open ship) and one level deep.
        ship(&db, 20, "PROJ-10", ShipState::Open); // now PROJ-10 has a branch to fork from
        let t11 = TicketRepo::get(&db, &TicketKey("PROJ-11".into())).unwrap().unwrap();
        assert_eq!(classify(&db, &t11, &armada(&db), true).unwrap(), Some(Classification::Stacked));
    }

    // ── blocks (keep our MRs' dependencies in step with the ticket graph) ─────────────

    #[test]
    fn blocks_wanted_for_our_child_on_its_blockers_open_ship() {
        let db = db();
        ticket(&db, "PROJ-1", TicketStatus::InQa, false, vec![]); // parent
        ticket(&db, "PROJ-2", TicketStatus::InProgress, false, vec!["PROJ-1"]); // child blocked_by parent
        order_wt(&db, "PROJ-2", true, Some("/wt")); // the child is OURS (dispatched); the blocker PROJ-1 need not be
        ship(&db, 101, "PROJ-1", ShipState::Open);
        ship(&db, 102, "PROJ-2", ShipState::Open);
        assert_eq!(wanted_blocks(&db).unwrap(), vec![(102, vec![101])]);
    }

    #[test]
    fn blocks_never_wanted_on_a_child_we_dont_own() {
        // An ally's MR (no order — we didn't dispatch it) is never given a dependency, even with a
        // blocked_by edge + both ships open. The fleet only sets a merge gate on an MR it owns.
        let db = db();
        ticket(&db, "PROJ-1", TicketStatus::InQa, false, vec![]);
        ticket(&db, "PROJ-2", TicketStatus::InProgress, false, vec!["PROJ-1"]); // allied child — NO order
        ship(&db, 101, "PROJ-1", ShipState::Open);
        ship(&db, 102, "PROJ-2", ShipState::Open);
        assert!(wanted_blocks(&db).unwrap().is_empty());
    }

    #[test]
    fn a_gated_child_with_no_blocker_left_is_still_tended() {
        // Its blocker's MR closed and the edge moved on: the forge still gates it, so it's listed
        // with no parent — sync_blocks then drops the dead dependency.
        let db = db();
        ticket(&db, "PROJ-2", TicketStatus::InProgress, false, vec![]);
        order_wt(&db, "PROJ-2", true, Some("/wt"));
        ship(&db, 102, "PROJ-2", ShipState::Open);
        assert!(wanted_blocks(&db).unwrap().is_empty(), "no edge, not gated → nothing to tend");
        let child = db.armada().unwrap().into_iter().find(|s| s.iid == 102).unwrap();
        let mut snap = ShipRepo::snapshot(&db, &child.id).unwrap().unwrap();
        snap.merge_status = Some("merge_request_blocked".into());
        db.sync_snapshot(&snap).unwrap();
        assert_eq!(wanted_blocks(&db).unwrap(), vec![(102, vec![])]);
    }

    #[test]
    fn blocks_skip_a_cycle_closing_edge() {
        let db = db();
        // Both tickets first (no edges), then the mutual edges — a 2-cycle can't be built with
        // inline blocked_by (each would reference the other before it exists → FK violation).
        ticket(&db, "PROJ-1", TicketStatus::InProgress, false, vec![]);
        ticket(&db, "PROJ-2", TicketStatus::InProgress, false, vec![]);
        TicketRepo::set_blocked_by(&db, &TicketKey("PROJ-1".into()), &[TicketKey("PROJ-2".into())]).unwrap();
        TicketRepo::set_blocked_by(&db, &TicketKey("PROJ-2".into()), &[TicketKey("PROJ-1".into())]).unwrap();
        order_wt(&db, "PROJ-1", true, Some("/wt")); // both ours, so the cycle guard is the only skip-reason tested
        order_wt(&db, "PROJ-2", true, Some("/wt"));
        ship(&db, 101, "PROJ-1", ShipState::Open);
        ship(&db, 102, "PROJ-2", ShipState::Open);
        assert_eq!(wanted_blocks(&db).unwrap(), vec![(101, vec![]), (102, vec![])], "a 2-cycle declares neither");
    }

    #[test]
    fn a_live_tick_tends_blocks_a_dry_run_doesnt() {
        let db = db();
        ticket(&db, "PROJ-1", TicketStatus::InQa, false, vec![]);
        ticket(&db, "PROJ-2", TicketStatus::InProgress, false, vec!["PROJ-1"]);
        order_wt(&db, "PROJ-2", true, Some("/wt"));
        ship(&db, 101, "PROJ-1", ShipState::Open);
        ship(&db, 102, "PROJ-2", ShipState::Open);
        let (tracker, host, forge) = (FakeTracker::new(), FakeHost::new(), FakeForge::new());
        tick(&db, &tracker, &host, &forge, &[], "/project", true).unwrap();
        assert!(forge.blocks.borrow().is_empty(), "a dry run touches no forge");
        tick(&db, &tracker, &host, &forge, &[], "/project", false).unwrap();
        assert_eq!(*forge.blocks.borrow(), vec![(102, vec![101])]);
    }

    // ── stand-down (the cold-cog session close) ──────────────────────────────────────

    #[test]
    fn a_dead_sailor_stands_down_rather_than_heals() {
        // A cold sailor whose tmux is gone (finished its sweep, or crashed) is stood down —
        // its session ended — NOT healed in place. The sweep trigger re-launches on a change.
        let db = db();
        ticket(&db, "PROJ-1", TicketStatus::InQa, false, vec![]);
        let cogs = vec![dead_cog("PROJ-1", CrewKind::Sailor)];
        assert_eq!(
            plan(&db, &cogs, true).unwrap(),
            vec![Action::StandDown { ticket: "PROJ-1".into(), kind: "sailor".into() }]
        );
    }

    #[test]
    fn an_escalated_sailor_is_left_alone() {
        // `escalated` is a deliberate hold (it waits for the operator): even Dead, it is neither
        // stood down nor re-launched over — the live-sailor lock holds the slot.
        let db = db();
        ticket(&db, "PROJ-1", TicketStatus::InQa, false, vec![]);
        let s = live_cog(&db, CrewKind::Sailor, "PROJ-1", Liveness::Dead);
        report(&db, &s, CogState::Sailor(SailorState::Escalated));
        assert!(plan(&db, &[s], true).unwrap().is_empty());
    }

    #[test]
    fn a_dead_anchored_sailor_is_left_alone() {
        // The operator anchored it: a hold, not a finish — never stand it down (weigh resumes it).
        let db = db();
        ticket(&db, "PROJ-1", TicketStatus::InQa, false, vec![]);
        let mut cog = dead_cog("PROJ-1", CrewKind::Sailor);
        cog.session.anchored_at = Some("t1".into());
        assert!(plan(&db, &[cog], true).unwrap().is_empty());
    }

    #[test]
    fn executes_stand_down_ends_the_sailor_session_and_kills_tmux() {
        let db = db();
        ticket(&db, "PROJ-1", TicketStatus::InQa, false, vec![]);
        let s = live_cog(&db, CrewKind::Sailor, "PROJ-1", Liveness::Dead); // finished its sweep
        let tracker = FakeTracker::new();
        let host = FakeHost::new();
        assert_eq!(tick(&db, &tracker, &host, &FakeForge::new(), &[s], "/project", false).unwrap(), 1);

        let sessions = SessionRepo::for_ticket(&db, &TicketKey("PROJ-1".into())).unwrap();
        assert!(sessions.iter().all(|s| s.ended_at.is_some()), "the sailor stood down");
        assert!(host.spawned.borrow().is_empty(), "stand-down never respawns (that's heal)");
        assert_eq!(host.killed.borrow().as_slice(), &["sailor-PROJ-1".to_string()]);
        assert!(tracker.moves.borrow().is_empty(), "stand-down touches no Jira");
        let ev = db.since(Seq(0)).unwrap();
        assert!(ev.iter().any(|e| e.kind == EventKind::SessionEnded && e.payload["reason"] == "stand_down"));
    }

    #[test]
    fn a_live_sailor_that_reported_sleeping_stands_down() {
        // Graceful cold-loop reap: a sailor that swept clean reports `sleeping` and idles —
        // still Active, because `claude --agent` doesn't self-exit. The report, not a dead
        // tmux, stands it down so the sweep trigger re-fires on the next change.
        let db = db();
        ticket(&db, "PROJ-1", TicketStatus::InQa, false, vec![]);
        let s = live_cog(&db, CrewKind::Sailor, "PROJ-1", Liveness::Active);
        report(&db, &s, CogState::Sailor(SailorState::Sleeping));
        assert_eq!(
            plan(&db, &[s], true).unwrap(),
            vec![Action::StandDown { ticket: "PROJ-1".into(), kind: "sailor".into() }]
        );
    }

    #[test]
    fn a_live_sailor_mid_sweep_is_left_running() {
        // Only the `sleeping` terminal report reaps a LIVE cog — a sailor still working (no report
        // yet, Active) is left to finish.
        let db = db();
        ticket(&db, "PROJ-1", TicketStatus::InQa, false, vec![]);
        let s = live_cog(&db, CrewKind::Sailor, "PROJ-1", Liveness::Active);
        assert!(plan(&db, &[s], true).unwrap().is_empty());
    }

    #[test]
    fn a_sleeping_but_anchored_sailor_is_left_alone() {
        // Operator anchor outranks the sleeping reap — a hold is a hold (weigh resumes it).
        let db = db();
        ticket(&db, "PROJ-1", TicketStatus::InQa, false, vec![]);
        let mut s = live_cog(&db, CrewKind::Sailor, "PROJ-1", Liveness::Active);
        report(&db, &s, CogState::Sailor(SailorState::Sleeping));
        s.session.anchored_at = Some("t1".into());
        assert!(plan(&db, &[s], true).unwrap().is_empty());
    }

    #[test]
    fn reaps_a_merged_ship_with_no_live_session_while_the_order_stands() {
        // Cold model: the sailor stood down (no live session) before the MR merged. The open
        // order is the "not yet reaped" marker → reap still drives Jira Done + retires it.
        let db = db();
        ticket(&db, "PROJ-1", TicketStatus::InQa, false, vec![]);
        order_wt(&db, "PROJ-1", true, Some("/wt"));
        ship(&db, 10, "PROJ-1", ShipState::Merged);
        assert_eq!(plan(&db, &[], true).unwrap(), vec![Action::Reap { ticket: "PROJ-1".into() }]);
    }

    // ── parent roll-up (converge rollup_parent) ─────────────────────────────────────

    #[test]
    fn reap_rolls_up_the_parent_when_all_subtasks_are_terminal() {
        let db = db();
        ticket(&db, "PROJ-101", TicketStatus::InQa, false, vec![]); // the sub-task being reaped
        let tracker = FakeTracker::new();
        *tracker.parent.borrow_mut() = Some(TicketKey("PROJ-100".into()));
        *tracker.subtasks.borrow_mut() = vec![TicketStatus::Done, TicketStatus::Done]; // all terminal
        let host = FakeHost::new();

        reap(&db, &tracker, &host, "/project", "PROJ-101").unwrap();

        let moves = tracker.moves.borrow();
        assert!(moves.contains(&("PROJ-101".to_string(), TicketStatus::Done)), "sub-task closed");
        assert!(moves.contains(&("PROJ-100".to_string(), TicketStatus::Done)), "parent rolled up");
    }

    #[test]
    fn reap_leaves_the_parent_open_while_a_sibling_is_unfinished() {
        let db = db();
        ticket(&db, "PROJ-101", TicketStatus::InQa, false, vec![]);
        let tracker = FakeTracker::new();
        *tracker.parent.borrow_mut() = Some(TicketKey("PROJ-100".into()));
        *tracker.subtasks.borrow_mut() = vec![TicketStatus::Done, TicketStatus::InProgress]; // one still going
        let host = FakeHost::new();

        reap(&db, &tracker, &host, "/project", "PROJ-101").unwrap();

        assert!(
            !tracker.moves.borrow().iter().any(|(k, _)| k == "PROJ-100"),
            "parent untouched until every sub-task is terminal"
        );
    }

    // ── captain (the cold-looped reconcile_foremen port) ────────────────────────────

    #[test]
    fn launches_a_captain_for_an_active_convoy_with_a_ready_undispatched_ticket() {
        let db = db();
        db.open(&ConvoyKey("PROJ-100".into()), None, "t1").unwrap();
        convoy_ticket(&db, "PROJ-1", "PROJ-100", TicketStatus::Todo); // ready, un-dispatched → work
        assert_eq!(
            plan(&db, &[], true).unwrap(),
            vec![Action::LaunchCaptain { convoy: "PROJ-100".into() }]
        );
    }

    #[test]
    fn no_captain_launch_for_an_anchored_convoy() {
        // The operator anchored the convoy (its hold lives on the convoy row) → no captain relaunch,
        // even with a ready-undispatched ticket waiting.
        let db = db();
        db.open(&ConvoyKey("PROJ-100".into()), None, "t1").unwrap();
        convoy_ticket(&db, "PROJ-1", "PROJ-100", TicketStatus::Todo);
        ConvoyRepo::anchor(&db, &ConvoyKey("PROJ-100".into()), "t2").unwrap();
        assert!(plan(&db, &[], true).unwrap().is_empty());
    }

    #[test]
    fn launches_a_captain_to_close_a_fully_delivered_convoy() {
        let db = db();
        db.open(&ConvoyKey("PROJ-100".into()), None, "t1").unwrap();
        convoy_ticket(&db, "PROJ-1", "PROJ-100", TicketStatus::Done); // every ticket terminal → close
        assert_eq!(
            plan(&db, &[], true).unwrap(),
            vec![Action::LaunchCaptain { convoy: "PROJ-100".into() }]
        );
    }

    #[test]
    fn no_launch_when_the_convoy_is_all_in_flight() {
        // Dispatched + still working: nothing ready to dispatch, nothing terminal to close → sleep.
        let db = db();
        db.open(&ConvoyKey("PROJ-100".into()), None, "t1").unwrap();
        convoy_ticket(&db, "PROJ-1", "PROJ-100", TicketStatus::InProgress);
        assert!(plan(&db, &[], true).unwrap().is_empty());
    }

    #[test]
    fn no_launch_when_a_captain_already_commands_the_convoy() {
        let db = db();
        db.open(&ConvoyKey("PROJ-100".into()), None, "t1").unwrap();
        convoy_ticket(&db, "PROJ-1", "PROJ-100", TicketStatus::Todo); // work waiting
        // an alive captain already holds the convoy → no second launch.
        assert!(plan(&db, &[captain_cog("PROJ-100")], true).unwrap().is_empty());
    }

    #[test]
    fn stands_down_a_captain_whose_convoy_is_no_longer_active() {
        let db = db();
        // convoy never active (closed/gone) + a live captain → stand it down.
        assert_eq!(
            plan(&db, &[captain_cog("PROJ-100")], true).unwrap(),
            vec![Action::StandDownCaptain { convoy: "PROJ-100".into() }]
        );
    }

    #[test]
    fn a_captain_that_reported_done_stands_down_and_relaunches_on_waiting_work() {
        // `claude --agent` doesn't self-exit: the captain's `done` report, not a dead tmux, ends its
        // pass — and frees the convoy's slot for a fresh pass in the same plan.
        let db = db();
        db.open(&ConvoyKey("PROJ-100".into()), None, "t1").unwrap();
        convoy_ticket(&db, "PROJ-1", "PROJ-100", TicketStatus::Todo); // work waiting
        let id = SessionRepo::start(&db, &captain_cog("PROJ-100").session).unwrap().expect("session start");
        let cog = Cog { session: SessionRepo::get(&db, &id).unwrap().unwrap(), liveness: Liveness::Active };
        assert!(plan(&db, std::slice::from_ref(&cog), true).unwrap().is_empty(), "mid-pass, it holds the slot");
        report(&db, &cog, CogState::Captain(CaptainState::Done));
        assert_eq!(
            plan(&db, &[cog], true).unwrap(),
            vec![
                Action::StandDownCaptain { convoy: "PROJ-100".into() },
                Action::LaunchCaptain { convoy: "PROJ-100".into() },
            ]
        );
    }

    #[test]
    fn stands_down_a_dead_captain_so_the_trigger_can_relaunch() {
        let db = db();
        db.open(&ConvoyKey("PROJ-100".into()), None, "t1").unwrap();
        let mut cog = captain_cog("PROJ-100");
        cog.liveness = Liveness::Dead; // finished its pass (or crashed) on a still-active convoy
        // stand-down fires; no relaunch this tick (the Dead cog still blocks via the lock until ended,
        // and there's no pending work in the empty convoy).
        assert_eq!(
            plan(&db, &[cog], true).unwrap(),
            vec![Action::StandDownCaptain { convoy: "PROJ-100".into() }]
        );
    }

    #[test]
    fn executes_launch_captain_records_a_ticketless_session_and_injects_the_convoy_env() {
        let db = db();
        db.open(&ConvoyKey("PROJ-100".into()), None, "t1").unwrap();
        convoy_ticket(&db, "PROJ-1", "PROJ-100", TicketStatus::Todo);
        let tracker = FakeTracker::new();
        let host = FakeHost::new();
        assert_eq!(tick(&db, &tracker, &host, &FakeForge::new(), &[], "/project", false).unwrap(), 1);

        let live = db.live().unwrap();
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].kind, CrewKind::Captain);
        assert!(live[0].ticket.is_none(), "a captain is ticketless");
        assert_eq!(live[0].convoy, Some(ConvoyKey("PROJ-100".into())));
        // launched in the project checkout (no worktree), convoy identity injected via FLEET_CONVOY, no Jira.
        let spawned = host.spawned.borrow();
        assert_eq!(spawned.len(), 1);
        let (name, cwd, cmd) = &spawned[0];
        assert_eq!((name.as_str(), cwd.as_str()), ("captain-PROJ-100", "/project"));
        assert!(
            cmd.starts_with("FLEET_CONVOY=PROJ-100 claude --session-id ") && cmd.ends_with(" --agent captain"),
            "cmd was: {cmd}"
        );
        assert!(tracker.moves.borrow().is_empty());
    }

    #[test]
    fn executes_stand_down_captain_ends_the_session_and_kills_tmux() {
        let db = db();
        db.open(&ConvoyKey("PROJ-100".into()), None, "t1").unwrap();
        convoy_ticket(&db, "PROJ-1", "PROJ-100", TicketStatus::Todo);
        let tracker = FakeTracker::new();
        let host = FakeHost::new();
        tick(&db, &tracker, &host, &FakeForge::new(), &[], "/project", false).unwrap(); // launches the captain
        db.close(&ConvoyKey("PROJ-100".into()), "t2").unwrap(); // captain signs off → convoy inactive

        // live captain + inactive convoy → StandDownCaptain (the real session is found by convoy).
        assert_eq!(tick(&db, &tracker, &host, &FakeForge::new(), &[captain_cog("PROJ-100")], "/project", false).unwrap(), 1);
        assert!(db.live().unwrap().is_empty(), "the captain session ended");
        assert!(host.killed.borrow().iter().any(|n| n == "captain-PROJ-100"));
        assert!(tracker.moves.borrow().is_empty(), "a captain owns no ticket, so no Jira");
    }
}
