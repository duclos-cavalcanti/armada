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
    /// Record the snapshot fingerprint the last sailor was dispatched against (the sweep lock).
    /// `None` = never swept, so the next ungated pass re-sweeps (a hold's disarm needs a re-arm).
    fn set_swept(&self, id: &ShipId, fingerprint: Option<&str>) -> Result<()>;
    /// Record the `head_sha` the last lookout reviewed (the review cold-loop change-detector).
    fn set_reviewed(&self, id: &ShipId, head_sha: &str) -> Result<()>;
}

pub trait OrderRepo {
    /// Submit an order to the dispatch queue (upsert; one active order per ticket).
    fn dispatch(&self, o: &Order) -> Result<()>;
    fn get(&self, key: &TicketKey) -> Result<Option<Order>>;
    fn queue(&self) -> Result<Vec<Order>>;
    fn cancel(&self, key: &TicketKey) -> Result<()>;
}

pub trait ConvoyRepo {
    /// Open a convoy (operator fact) — idempotent create; re-opening a closed convoy clears
    /// `ended_at`. Appends `ConvoyOpened`.
    fn open(&self, key: &ConvoyKey, title: Option<&str>, now: &str) -> Result<()>;
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
}

/// The singleton lookout's control row — the allied-ship review cog is ticketless + convoyless, so its
/// one operator-settable fact (the anchor) lives in its own one-row table, not on any session.
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

pub trait EscalationRepo {
    fn raise(&self, e: &Escalation) -> Result<EscalationId>;
    fn resolve(&self, id: &EscalationId, at: &str, resolution: &str) -> Result<()>;
    fn list_open(&self) -> Result<Vec<Escalation>>;
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
    /// Can this forge stack MRs — declare one MR blocked by another (`block_on`)? When false the
    /// fleet never dispatches a stacked child: it waits for its parent to merge instead.
    fn stacks(&self) -> bool;
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
    /// Declare a GitLab MR dependency: `child` is blocked by `parent` (GitLab then gates the
    /// child's merge + merge-train until the parent merges). Idempotent — an already-declared
    /// edge is a success no-op. The reconcile BLOCK pass converges these from the ticket graph.
    fn block_on(&self, child_iid: i64, parent_iid: i64) -> Result<()>;
    fn arm_merge(&self, iid: i64) -> Result<()>;
    /// Cancel an armed auto-merge, so the MR won't merge on its own. Idempotent — an unarmed MR is
    /// a success no-op. A hold calls it: an armed MR lives on the forge, beyond the fleet's gates.
    fn disarm(&self, iid: i64) -> Result<()>;
    /// Mark the MR draft (`true`: it can't merge, armed or not) or ready (`false`). Idempotent — an
    /// MR already in that state is a success no-op. Errors where the forge has no drafts.
    fn set_draft(&self, iid: i64, draft: bool) -> Result<()>;
    /// Open a new MR from `draft` (the source branch is already pushed), assigned to the fleet's
    /// user. Returns its URL; the next sync picks the MR up into the armada.
    fn open_mr(&self, draft: &MrDraft) -> Result<String>;
    /// The MR's CI: its status and every failed job with its log tail (child pipelines included).
    fn ci(&self, iid: i64) -> Result<CiReport>;
    /// The ref an MR's head can be fetched by from `origin` (GitHub `pull/N/head`, GitLab
    /// `merge-requests/N/head`) — how a reviewer gets an MR it didn't author.
    fn fetch_ref(&self, iid: i64) -> String;
    /// Post one review comment on the MR, then approve it when `approve` (the comment stands even
    /// if the forge refuses the approval, e.g. on your own MR).
    fn post_review(&self, iid: i64, body: &str, approve: bool) -> Result<()>;
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
    /// The ticket's parent story (`None` if it isn't a sub-task) — for parent roll-up.
    fn parent_of(&self, key: &TicketKey) -> Result<Option<TicketKey>>;
    /// A parent's native sub-task statuses (empty = not a sub-task container, e.g. an Epic).
    fn subtasks(&self, parent: &TicketKey) -> Result<Vec<TicketStatus>>;
    /// The ticket's full text — its board facts, type, and description (`fleetc ticket <KEY>`).
    fn read(&self, key: &TicketKey) -> Result<TicketDetail>;
    /// Create a ticket in To Do from `draft`, with its "is blocked by" edges; returns its key.
    fn create(&self, draft: &TicketDraft) -> Result<TicketKey>;
    /// Record that `key` is blocked by `by` (idempotent).
    fn block(&self, key: &TicketKey, by: &TicketKey) -> Result<()>;
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
