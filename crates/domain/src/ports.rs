//! Ports (PLAN §3.1) — the traits the outside plugs into.
//!
//! Two families, both *driven* (the daemon calls outward):
//! - **repository ports** — persistence, implemented by `ledger`.
//! - **external ports** — `Forge`/`Tracker`/`SessionHost`, implemented by drivers
//!   (glab/jira/tmux) in a later increment.
//!
//! `domain` depends on nothing concrete; swapping the stack = a new impl here.

use crate::*;
use std::path::Path;

pub type Result<T> = anyhow::Result<T>;

// ── repository ports (implemented by `ledger`) ──────────────────────────────────

pub trait TicketRepo {
    fn upsert(&self, t: &Ticket) -> Result<()>;
    fn get(&self, key: &TicketKey) -> Result<Option<Ticket>>;
    /// The board — all sprint tickets (upsert-filled by the tracker).
    fn board(&self) -> Result<Vec<Ticket>>;
    /// Replace the ticket's `is blocked by` edges.
    fn set_blocked_by(&self, key: &TicketKey, blockers: &[TicketKey]) -> Result<()>;
    /// Hold (`held=true`) or release (`held=false`) the ticket — a fleet-local decision
    /// (the captain's groom-gate or the operator) that `is_ready` honours, keeping a held
    /// ticket off the dispatch frontier. Logs `TicketHeld`(with `reason`)/`TicketReleased`
    /// CDC in the same txn; a no-op change (already in that state) logs nothing.
    fn set_held(&self, key: &TicketKey, held: bool, reason: Option<&str>, at: &str) -> Result<()>;
    /// Mark the ticket for a hand (`true`: the captain dispatches it to one) or for a worker.
    fn set_hand(&self, key: &TicketKey, hand: bool, at: &str) -> Result<()>;
    fn is_hand(&self, key: &TicketKey) -> Result<bool>;
    /// Upsert the tracker-sourced fields (title/assignee/status/convoy + synced_at),
    /// PRESERVING our own `held` + `created_at`. Inserts with defaults if new.
    fn sync(&self, t: &TrackerTicket, now: &str) -> Result<()>;
}

pub trait SessionRepo {
    /// Record a new live session. Returns `None` when the `one_live_crew_per_ticket` lock
    /// already holds the slot (a live cog of this kind exists) — a benign skip, not an error.
    fn start(&self, s: &Session) -> Result<Option<SessionId>>;
    fn get(&self, id: &SessionId) -> Result<Option<Session>>;
    fn for_ticket(&self, key: &TicketKey) -> Result<Vec<Session>>;
    fn live(&self) -> Result<Vec<Session>>; // ended_at IS NULL
    fn end(&self, id: &SessionId, at: &str) -> Result<()>;
    /// Drop anchor on a live session (the operator "stop & hold") — sets `anchored_at`.
    /// `heal` won't respawn an anchored session. No-op (no event) if already anchored.
    fn anchor(&self, id: &SessionId, at: &str) -> Result<()>;
    /// Weigh anchor (get under way) — clears `anchored_at`. No-op if not anchored.
    fn weigh(&self, id: &SessionId, at: &str) -> Result<()>;
}

pub trait ShipRepo {
    fn upsert(&self, s: &Ship) -> Result<ShipId>;
    fn get_by_iid(&self, iid: i64) -> Result<Option<Ship>>;
    /// The armada — all open ships (ours + allies).
    fn armada(&self) -> Result<Vec<Ship>>;
    fn sync_snapshot(&self, snap: &ShipSnapshot) -> Result<()>;
    fn snapshot(&self, id: &ShipId) -> Result<Option<ShipSnapshot>>;
    /// When the ship's review threads last moved (opened or resolved), as the syncs saw it: a sailor
    /// waits for them to hold still. `None` = never moved since its first snapshot.
    fn threads_changed_at(&self, id: &ShipId) -> Result<Option<String>>;
    /// Record the snapshot fingerprint the last sailor was dispatched against (the sweep lock).
    /// `None` = never swept, so the next ungated pass re-sweeps (a hold's disarm needs a re-arm).
    fn set_swept(&self, id: &ShipId, fingerprint: Option<&str>) -> Result<()>;
    /// Record the `head_sha` the last lookout reviewed (the review cold-loop change-detector).
    /// `None` = no review stands, so the next pass reviews it again (a lookout died mid-review).
    fn set_reviewed(&self, id: &ShipId, head_sha: Option<&str>) -> Result<()>;
    /// A sailor sweep was launched on it: one review round more.
    fn count_round(&self, id: &ShipId) -> Result<()>;
    /// Retire a ship — it left the open list, merged or closed (sets `ended_at`). Idempotent; a
    /// reopened MR's next upsert clears it again.
    fn retire(&self, id: &ShipId, at: &str) -> Result<()>;
}

pub trait OrderRepo {
    /// Submit an order to the dispatch queue (upsert; one active order per ticket).
    fn dispatch(&self, o: &Order) -> Result<()>;
    /// Re-issue a ticket's order for a rework — a worker (`auto_impl`) or a hand — re-dated `at`,
    /// so a builder is due again in its worktree. Errors when the ticket has no order.
    fn rework(&self, key: &TicketKey, auto_impl: bool, at: &str) -> Result<()>;
    fn get(&self, key: &TicketKey) -> Result<Option<Order>>;
    fn queue(&self) -> Result<Vec<Order>>;
    fn cancel(&self, key: &TicketKey) -> Result<()>;
}

pub trait ConvoyRepo {
    /// Open a convoy (operator fact) — idempotent create; re-opening a closed convoy clears
    /// `ended_at`. Appends `ConvoyOpened`.
    fn open(&self, key: &ConvoyKey, title: Option<&str>, now: &str) -> Result<()>;
    /// Set the convoy's plan (`Some`) or clear it (`None`). Appends `ConvoyPlanned` with the new plan
    /// (`null`: cleared) — the plan's history. No-op (no event) if unchanged.
    fn set_plan(&self, key: &ConvoyKey, plan: Option<&str>, at: &str) -> Result<()>;
    fn plan(&self, key: &ConvoyKey) -> Result<Option<String>>;
    /// Close a convoy (the captain's "all delivered" judgment) — sets `ended_at`, which
    /// reconcile reads to reap the captain. Appends `ConvoyClosed`.
    fn close(&self, key: &ConvoyKey, at: &str) -> Result<()>;
    /// Anchor a convoy — the operator "hold the captain": sets `anchored_at`, which reconcile's
    /// captain launch gate reads to skip the relaunch (the hold lives on the convoy, the trigger
    /// source, not the cold session). Appends `ConvoyAnchored`. No-op (no event) if already anchored.
    fn anchor(&self, key: &ConvoyKey, at: &str) -> Result<()>;
    /// Weigh a convoy — lift the hold: clears `anchored_at`. Appends `ConvoyWeighed`. No-op if not anchored.
    fn weigh(&self, key: &ConvoyKey, at: &str) -> Result<()>;
    /// The active convoys (`ended_at IS NULL`) — what captain staffing keys off.
    fn active(&self) -> Result<Vec<Convoy>>;
    /// Put a ticket in `convoy` — its only convoy: one from another epic leaves its epic's convoy (for
    /// the fleet; returned, so the caller can say so). A no-op if it's already a member. Errs if it was
    /// added to another convoy (remove it there first) or isn't on the board.
    fn add_member(&self, convoy: &ConvoyKey, ticket: &TicketKey, at: &str) -> Result<Option<ConvoyKey>>;
    /// Take an added ticket out of `convoy` (it returns to its epic's). Errs if it wasn't added there.
    fn remove_member(&self, convoy: &ConvoyKey, ticket: &TicketKey) -> Result<()>;
    /// Every ticket added to a convoy — sync keeps the off-board ones fresh.
    fn added_members(&self) -> Result<Vec<TicketKey>>;
}

/// The lookouts' control row — the hold over the whole review loop (the anchor) lives in its own
/// one-row table, not on any one lookout's session.
pub trait LookoutRepo {
    /// Anchor the lookout — hold the review loop: sets `anchored_at`, which reconcile's lookout
    /// launch gate reads to skip the relaunch. Appends `LookoutAnchored`. No-op if already anchored.
    fn anchor(&self, at: &str) -> Result<()>;
    /// Weigh the lookout — lift the hold: clears `anchored_at`. Appends `LookoutWeighed`. No-op if not anchored.
    fn weigh(&self, at: &str) -> Result<()>;
    /// Is the lookout anchored? — the launch gate's read.
    fn anchored(&self) -> Result<bool>;
}

pub trait ReportRepo {
    /// Record a crew member's latest self-report (replaces the prior latest for that
    /// session) and append a `StatusReported` event — the two in one txn, so the
    /// latest row and the history never diverge. The caller supplies a pre-parsed
    /// `Report`, so an invalid state can't reach here (the enum was the lint).
    fn record(&self, r: &Report) -> Result<()>;
    /// The session's latest self-report, widened back to a typed `CogState` via the
    /// session's `kind`. `None` = the session has never reported.
    fn latest(&self, session: &SessionId) -> Result<Option<Report>>;
}

pub trait InspectionRepo {
    /// Record a verdict on a cog's escalation and append an `EscalationInspected` event, in one txn.
    fn inspect(&self, i: &Inspection) -> Result<()>;
    /// The session's latest inspection; `None` = never inspected.
    fn inspection(&self, session: &SessionId) -> Result<Option<Inspection>>;
}

pub trait CaseRepo {
    /// The session's open case: the escalation its cog stands on now. Reports open, join and settle
    /// cases (`ReportRepo::record`); ending the session settles its open one.
    fn open_case(&self, session: &SessionId) -> Result<Option<Case>>;
    fn case(&self, id: &CaseId) -> Result<Option<Case>>;
    /// A convoy's cases, newest first; with no convoy, the cases on `ticket` raised outside any.
    fn cases(&self, convoy: Option<&ConvoyKey>, ticket: &TicketKey) -> Result<Vec<Case>>;
    /// The case's latest verdict; `None` = not inspected yet.
    fn verdict(&self, id: &CaseId) -> Result<Option<Inspection>>;
    /// Record the operator's ruling on a confirmed case, once, with an `EscalationRuled` event. It
    /// settles when its cog moves on, as any case does.
    fn rule(&self, id: &CaseId, ruling: &Ruling) -> Result<()>;
    /// Record a ruling with no escalation behind it, on `ticket`: a new case, settled at once.
    fn rule_alone(&self, ticket: &TicketKey, ruling: &Ruling) -> Result<CaseId>;
}

pub trait ProposalRepo {
    fn propose(&self, p: &Proposal) -> Result<ProposalId>;
    fn get(&self, id: &ProposalId) -> Result<Option<Proposal>>;
    /// Settle an open proposal: `accepted_as` = the ticket it became or was, `None` = dropped. Errs
    /// if it isn't open (already settled, or no such id).
    fn settle(&self, id: &ProposalId, at: &str, accepted_as: Option<&TicketKey>) -> Result<()>;
    /// The open proposals, oldest first.
    fn pending(&self) -> Result<Vec<Proposal>>;
}

pub trait EventLog {
    /// Append one change-log entry; returns the assigned `seq`.
    fn append(
        &self,
        created_at: &str,
        kind: EventKind,
        entity: EntityKind,
        entity_ref: &str,
        payload: &serde_json::Value,
    ) -> Result<Seq>;
    /// Entries after `seq` (the follow cursor).
    fn since(&self, seq: Seq) -> Result<Vec<Event>>;
}

// ── external ports (implemented by drivers — a later increment) ──────────────────

pub trait Forge {
    /// Preflight: verify this adapter can work — its CLI is installed, authenticated, and reaches
    /// the configured target. `Ok` carries a one-line confirmation for the checklist.
    fn preflight(&self) -> Result<String>;
    /// Can this forge stack MRs — declare one MR blocked by another (`sync_blocks`)? When false the
    /// fleet never dispatches a stacked child: it waits for its parent to merge instead.
    fn stacks(&self) -> bool;
    /// Does `armada` already carry each MR exact (threads, approvals, pipeline, blocks)? When false,
    /// sync re-reads ours one by one with `snapshot`.
    fn armada_is_exact(&self) -> bool;
    /// Every open MR (ours + allies = the armada), each as a `ForgeShip` DTO.
    fn armada(&self) -> Result<Vec<ForgeShip>>;
    fn snapshot(&self, iid: i64) -> Result<ForgeShip>;
    /// The MR's fresh review surface from the discussions endpoint — the merge-blocking
    /// open-thread count + the floating notes (the bash `mr-open-threads`/`mr-floating-notes`
    /// pair, now one fetch). Fresh on demand: a cog resolving review needs current state.
    fn review_state(&self, iid: i64) -> Result<ReviewState>;
    /// Reply on one review thread, and resolve it when `resolve` (the ask is addressed).
    fn reply(&self, iid: i64, thread: &str, body: &str, resolve: bool) -> Result<()>;
    fn rebase(&self, iid: i64) -> Result<()>;
    /// Keep `child`'s MR dependencies in step: drop any on an MR that closed unmerged (the forge
    /// would gate the child on it forever), then declare each of `parents` not yet declared (the
    /// child then can't merge until they do). Other live dependencies stay. Idempotent; says how
    /// many it changed. Reconcile tends these from the ticket graph.
    fn sync_blocks(&self, child_iid: i64, parents: &[i64]) -> Result<usize>;
    fn arm_merge(&self, iid: i64) -> Result<()>;
    /// Cancel an armed auto-merge, so the MR won't merge on its own. Idempotent — an unarmed MR is
    /// a success no-op. A hold calls it: an armed MR lives on the forge, beyond the fleet's gates.
    fn disarm(&self, iid: i64) -> Result<()>;
    /// Close the MR unmerged (its branch stays). Idempotent — a closed MR is a success no-op.
    fn close_mr(&self, iid: i64) -> Result<()>;
    /// Mark the MR draft (`true`: it can't merge, armed or not) or ready (`false`). Idempotent — an
    /// MR already in that state is a success no-op. Errors where the forge has no drafts.
    fn set_draft(&self, iid: i64, draft: bool) -> Result<()>;
    /// Open a new MR from `draft` (the source branch is already pushed), assigned to the fleet's
    /// user. Returns its URL; the next sync picks the MR up into the armada.
    fn open_mr(&self, draft: &MrDraft) -> Result<String>;
    /// Rewrite an open MR's title + description — a relaunch, after a rework changed its ticket.
    fn describe_mr(&self, iid: i64, title: &str, description: &str) -> Result<()>;
    /// The MR's CI: its status and every failed job with its log tail (child pipelines included).
    fn ci(&self, iid: i64) -> Result<CiReport>;
    /// The ref an MR's head can be fetched by from `origin` (GitHub `pull/N/head`, GitLab
    /// `merge-requests/N/head`) — how a reviewer gets an MR it didn't author.
    fn fetch_ref(&self, iid: i64) -> String;
    /// Post one review comment on the MR, then approve it when `approve` (the comment stands even
    /// if the forge refuses the approval, e.g. on your own MR).
    fn post_review(&self, iid: i64, body: &str, approve: bool) -> Result<()>;
    /// Open one resolvable review thread on the MR (a finding its author must answer and resolve).
    /// A forge without top-level threads refuses.
    fn post_thread(&self, iid: i64, _body: &str) -> Result<()> {
        anyhow::bail!("!{iid}: this forge has no top-level review threads")
    }
    /// The paths each MR changes, every iid at once where the forge allows (one query on GitLab):
    /// what a lookout's crossing check holds a review against, with no branch fetched.
    fn changed_paths(&self, _iids: &[i64]) -> Result<Vec<(i64, Vec<String>)>> {
        anyhow::bail!("this forge can't list an MR's changed paths")
    }
    /// The MR's diff of one file — its hunks, as the forge shows them.
    fn file_diff(&self, iid: i64, _path: &str) -> Result<String> {
        anyhow::bail!("!{iid}: this forge can't show an MR's diff")
    }
    /// Re-run the MR's failed CI jobs (a transient failure's fix — no commit).
    fn retry_ci(&self, iid: i64) -> Result<()>;
    /// Hand our MR to the team's review: mark it ready (a draft is the pre-review state), resolve
    /// the reviewer set (GitLab: the approval rules' approvers, minus the author), assign them, and
    /// return the set — empty where the forge requests review itself (GitHub's CODEOWNERS on ready). Called once at handoff — the MR may not be in the ledger yet, so the forge
    /// resolves the iid from the ticket's branch itself.
    fn request_review(&self, ticket: &TicketKey) -> Result<Vec<String>>;
}

pub trait Tracker {
    /// Preflight: verify this adapter can work — its CLI is installed, authenticated, and reaches
    /// the configured target. `Ok` carries a one-line confirmation for the checklist.
    fn preflight(&self) -> Result<String>;
    /// The board — the tickets the fleet works from (Jira: the open sprint; filesystem: every file).
    fn board(&self) -> Result<Vec<TrackerTicket>>;
    fn get(&self, key: &TicketKey) -> Result<TrackerTicket>;
    fn transition(&self, key: &TicketKey, to: TicketStatus) -> Result<()>;
    /// True when the ticket is assigned to someone OTHER than the operator — the claim gate
    /// (never stomp a peer). `false` = unassigned or already ours, so it's safe to claim.
    fn assigned_to_peer(&self, key: &TicketKey) -> Result<bool>;
    /// Mark the ticket as the operator's active work, after the In Progress transition locked it
    /// (Jira: assign to me, add the claim label, join the active sprint; filesystem: assign to me).
    /// Best-effort follow-ons: try every step, then report the ones that failed (idempotent).
    fn claim(&self, key: &TicketKey) -> Result<()>;
    /// Return the ticket to the backlog, undoing a claim: To Do, unassigned, the claim label off,
    /// out of the active sprint (Jira's backlog). Best-effort like `claim`: try every step, then
    /// report the ones that failed (idempotent).
    fn unclaim(&self, key: &TicketKey) -> Result<()>;
    /// The ticket's parent story (`None` if it isn't a sub-task) — for parent roll-up.
    fn parent_of(&self, key: &TicketKey) -> Result<Option<TicketKey>>;
    /// The tickets whose parent is `parent`, whatever their sprint: a story's sub-tasks, an epic's
    /// (a convoy's) tickets.
    fn children(&self, parent: &TicketKey) -> Result<Vec<TrackerTicket>>;
    /// The ticket's full text — its board facts, type, and description (`fleetc ticket <KEY>`).
    fn read(&self, key: &TicketKey) -> Result<TicketDetail>;
    /// Several tickets' full text, in one call where the tracker allows (one search on Jira); one
    /// `read` each otherwise. A key it can't read is left out.
    fn read_many(&self, keys: &[TicketKey]) -> Result<Vec<TicketDetail>> {
        Ok(keys.iter().filter_map(|k| self.read(k).ok()).collect())
    }
    /// Create a ticket in To Do from `draft`, with its "is blocked by" edges and labels; returns its key.
    fn create(&self, draft: &TicketDraft) -> Result<TicketKey>;
    /// Replace the ticket's description with `description` (markdown, as `read` returns it).
    fn edit(&self, key: &TicketKey, description: &str) -> Result<()>;
    /// Add (`on`) or remove a label on the ticket (idempotent).
    fn label(&self, key: &TicketKey, label: &str, on: bool) -> Result<()>;
    /// Add (`on`) or remove a `link` between `key` and `other` (idempotent): `BlockedBy` is "`key`
    /// is blocked by `other`"; `Relates` relates the two, both ways.
    fn link(&self, key: &TicketKey, other: &TicketKey, link: TicketLink, on: bool) -> Result<()>;
    /// Add a comment to the ticket — a decision's why, kept where the team reads the ticket.
    fn comment(&self, key: &TicketKey, body: &str) -> Result<()>;
}

pub trait SessionHost {
    /// Preflight: verify this adapter can work — its CLI is installed, authenticated, and reaches
    /// the configured target. `Ok` carries a one-line confirmation for the checklist.
    fn preflight(&self) -> Result<String>;
    fn spawn(&self, name: &str, cwd: &Path, cmd: &str) -> Result<()>;
    fn alive(&self, name: &str) -> Result<bool>;
    /// Last observed activity as a unix epoch (seconds); tmux reads `#{window_activity}`.
    /// Epoch (not RFC3339) because it's a transient liveness probe, never stored.
    fn last_activity(&self, name: &str) -> Result<Option<i64>>;
    fn kill(&self, name: &str) -> Result<()>;
    fn send(&self, name: &str, text: &str) -> Result<()>;
    fn list(&self) -> Result<Vec<String>>;
}
