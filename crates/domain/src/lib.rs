//! fleet domain — the pure shape of the ledger (PLAN §3.1).
//!
//! Entities + enums only. No sqlite, no tmux, no http — those live behind ports
//! in other crates. Timestamps are RFC3339 UTC strings for now (tighten to a
//! typed time later); the ledger stores them as TEXT either way.
//!
//! Discipline: FACTS are stored; status is DERIVED at read time. So `Session`
//! has no `state` (derived), a `Ship`'s mergeability is derived from a fresh
//! `ShipSnapshot`, and liveness is derived from a background activity poll.

use serde::{Deserialize, Serialize};

/// RFC3339 UTC. Alias so we can tighten to a typed time later without churn.
pub type Timestamp = String;

pub mod ports;

// ── identifiers ───────────────────────────────────────────────────────────────
// Synthetic integer PKs = `Id`; the ticket's natural string key = `TicketKey`.

macro_rules! id_newtype {
    ($(#[$m:meta])* $name:ident, $inner:ty) => {
        $(#[$m])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub $inner);
    };
}
id_newtype!(/// `PROJ-1234` — the work order's natural key: `<PREFIX>-<n>`, the one shape
            /// every tracker mints (Jira natively; others map onto it).
            TicketKey, String);

impl TicketKey {
    /// Parse a `<PREFIX>-<n>` key — an uppercase-led `[A-Z][A-Z0-9]*` prefix, a dash, digits.
    /// Used wherever a key enters (client input, ledger reads): never silently coerced.
    pub fn parse(s: &str) -> anyhow::Result<Self> {
        anyhow::ensure!(key_shaped(s), "{s:?} is not a ticket key (expected <PREFIX>-<n>, e.g. PROJ-123)");
        Ok(Self(s.to_string()))
    }

    /// The first `<prefix>-<n>` key in free text (a branch name, an MR title), on a word
    /// boundary. Takes the tracker's prefix: the generic shape would also match `UTF-8`.
    pub fn find_in(text: &str, prefix: &str) -> Option<Self> {
        let needle = format!("{prefix}-");
        text.match_indices(&needle).find_map(|(i, _)| {
            let bounded = text[..i].chars().next_back().is_none_or(|c| !c.is_ascii_alphanumeric());
            let digits: String = text[i + needle.len()..].chars().take_while(char::is_ascii_digit).collect();
            (bounded && !digits.is_empty()).then(|| Self(format!("{needle}{digits}")))
        })
    }
}
id_newtype!(/// The convoy a ticket belongs to — the grouping a captain commands (Jira: the
            /// epic's key). NOT a ticket, but it shares the ticket key shape `<PREFIX>-<n>`, so
            /// every identifier in the fleet has one strict format.
            ConvoyKey, String);
impl ConvoyKey {
    /// Parse a convoy key — the same `<PREFIX>-<n>` shape as a ticket key.
    pub fn parse(s: &str) -> anyhow::Result<Self> {
        anyhow::ensure!(key_shaped(s), "{s:?} is not a convoy key (expected <PREFIX>-<n>, e.g. PROJ-100)");
        Ok(Self(s.to_string()))
    }
}

/// `<PREFIX>-<n>`: a key prefix, a dash, digits.
fn key_shaped(s: &str) -> bool {
    s.split_once('-')
        .is_some_and(|(prefix, num)| is_key_prefix(prefix) && !num.is_empty() && num.bytes().all(|b| b.is_ascii_digit()))
}

/// A key prefix: uppercase-led `[A-Z][A-Z0-9]*` (a Jira project key; a tracker's configured prefix).
pub fn is_key_prefix(p: &str) -> bool {
    p.starts_with(|c: char| c.is_ascii_uppercase()) && p.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
}

id_newtype!(SessionId, i64);
id_newtype!(ShipId, i64);
id_newtype!(ProposalId, i64);
id_newtype!(CaseId, i64);
id_newtype!(/// Monotonic event cursor (`GET /events?since=`).
            Seq, i64);

// ── enums ─────────────────────────────────────────────────────────────────────

/// The crew roles that run in a session (inspector retired; human→hand; captain
/// commands a convoy of sailors). An admiral session is one fleetd convenes on a ticket's
/// confirmed escalation, for the operator to rule with; it reports no states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CrewKind { Worker, Sailor, Hand, Captain, Lookout, Admiral }

// Per-cog self-reported state (ex `<cog>-status` schemas). Each cog reports its own
// vocabulary; `fleetc status` parses the report into the matching enum BEFORE the
// write (the enum IS the lint — a worker can't report a sailor state). Stored as the
// string form; reconcile reads it back and reacts.

/// A worker's lifecycle while it builds the ship. `waiting`: done, but a blocker hasn't merged —
/// it can't conclude until fleetd wakes it. `review`: its MR is pushed for a lookout to review —
/// it waits until fleetd wakes it with the review.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerState { Started, Implementing, Review, Waiting, Concluded, Escalated, Anchored }

/// What a sailor reports: its sweep is done (`sleeping`), or it holds for the operator (`escalated`,
/// `anchored`). Working, it reports nothing — fleetd sees the MR itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SailorState { Sleeping, Escalated, Anchored }

/// A hand's lifecycle (ad-hoc, operator-driven). `concluded` yields its MR to a sailor; `done`
/// closes a ticket whose work needed no MR (fleetd moves it to Done).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HandState { Awaiting, Implementing, Concluded, Done, Anchored }

/// The lookout's pass over one MR, ours or an ally's: `reviewed` (done, stand it down) or
/// `escalated` (wait for the operator in its pane).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LookoutState { Reviewed, Escalated }

/// The captain's pass over its convoy: `done` (frontier worked, stand it down) or `escalated` (a
/// delivered convoy it can't close, open proposals still bound for it: wait for the operator in its pane).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptainState { Done, Escalated }

/// A crew member's self-reported state, typed per cog. The `reports` table stores the
/// string (`as_str`); readers widen it back with the session's `kind` via `parse`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CogState {
    Worker(WorkerState),
    Sailor(SailorState),
    Hand(HandState),
    Captain(CaptainState),
    Lookout(LookoutState),
}

/// A ticket's workflow status — a cached mirror of the tracker (re-synced,
/// never trusted past `synced_at`). The tracker adapter maps the vendor onto this.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TicketStatus { Todo, Blocked, InProgress, InQa, Done, Cancelled }

/// A ship's lifecycle (cached from the forge). `Merged` = shipped into port.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShipState { Draft, Open, Merged, Closed }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PipelineStatus { Running, Success, Failed, Canceled, Manual }

/// Derived at read time from a background activity poll + the event log — NEVER
/// stored (a stored liveness rots the instant a cog dies).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Liveness { Active, Quiet, Idle, Dead }

/// What entity an event concerns (polymorphic; no FK).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntityKind { Session, Ticket, Ship, Escalation, Ticker, Convoy, Lookout }

/// The mutation an event records — a FACT, emitted by the mutating code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    SessionStarted, SessionEnded,
    StatusReported,
    Dispatched,
    TicketCreated, TicketSynced, TicketBlocked, TicketUnblocked,
    TicketHeld, TicketReleased, TicketDecommissioned, TicketRecalled, TicketAmended,
    ShipOpened, ShipSynced, ShipMerged, ShipClosed,
    EscalationInspected, EscalationRuled,
    ConvoyOpened, ConvoyClosed, ConvoyPlanned,
    SessionAnchored, SessionWeighed,
    ConvoyAnchored, ConvoyWeighed,
    LookoutAnchored, LookoutWeighed,
    TickRan,
    HealthChecked,
}

// ── entities (facts) ───────────────────────────────────────────────────────────

/// A running crew member (PLAN §3.1 `sessions`). Lifecycle = `created_at` +
/// `ended_at` (None = live). Everything else about the cog is derived or an event.
/// A worker/sailor binds to a `ticket`; a captain commands a `convoy` and carries no
/// ticket; a hand/standing singleton may have neither. The DB CHECKs enforce which.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Session {
    pub id: SessionId,
    pub kind: CrewKind,
    pub ticket: Option<TicketKey>,
    pub convoy: Option<ConvoyKey>,
    pub worktree_path: String,
    pub tmux_session: String,
    pub harness_session_id: Option<String>,
    pub created_at: Timestamp,
    pub ended_at: Option<Timestamp>,
    /// The operator anchor (ex "park"): set = this cog must stop & hold; `heal` never respawns
    /// an anchored session. Cleared by weighing anchor. NULL = under way.
    pub anchored_at: Option<Timestamp>,
    /// The allied ship a lookout reviews; `None` for every other cog.
    pub ship: Option<ShipId>,
}

impl Session {
    /// Claude Code's transcript folder for this session: `~/.claude/projects/<cwd>`, where `<cwd>`
    /// is the worktree's real path (symlinks resolved) with every non-alphanumeric char → `-`.
    pub fn transcript_dir(&self) -> Option<std::path::PathBuf> {
        let home = std::env::var_os("HOME")?;
        let cwd = std::fs::canonicalize(&self.worktree_path)
            .unwrap_or_else(|_| std::path::PathBuf::from(&self.worktree_path));
        let encoded: String =
            cwd.to_string_lossy().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
        Some(std::path::Path::new(&home).join(".claude/projects").join(encoded))
    }

    /// The transcript of the stored harness id — `None` for a pre-capture session (no stored id).
    pub fn transcript(&self) -> Option<std::path::PathBuf> {
        Some(self.transcript_dir()?.join(format!("{}.jsonl", self.harness_session_id.as_ref()?)))
    }
}

/// A crew member's latest self-report (PLAN §3.1 `reports`). Written via `fleetc
/// status` — the per-cog enum validates it at the write boundary — stored verbatim
/// (latest wins). reconcile reacts to `state`; full history lives in the event log
/// as `StatusReported`. `Deserialize` is omitted: constructed from rows + CLI args,
/// never parsed from JSON (the `cog` needed to widen `state` isn't in the payload).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Report {
    pub session: SessionId,
    pub state: CogState,
    pub last_action: Option<String>,
    pub reported_at: Timestamp,
}

/// The work order (PLAN §3.1 `tickets` = the board). Identity + edges + a cached
/// tracker mirror. Dispatch policy (auto_impl) lives on the `Order`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Ticket {
    pub key: TicketKey,
    /// The one convoy the fleet drives it in: the one it was added to, else its epic's.
    pub convoy: Option<ConvoyKey>,
    pub held: bool,
    pub created_at: Timestamp,
    /// The domain holds a vector; the ledger encodes it as the `ticket_blocks` table.
    pub blocked_by: Vec<TicketKey>,
    // cached tracker mirror (observation + when):
    pub title: Option<String>,
    pub assignee: Option<String>,
    pub status: Option<TicketStatus>,
    pub synced_at: Option<Timestamp>,
}

/// A convoy (PLAN §8 2026-10-16) — the operator-curated grouping a captain commands.
/// Existence is an operator fact (`open` → active); completion is the captain's judgment
/// (`close` → `ended_at` set, which reconcile reads to reap the captain). Loosely coupled
/// to `Ticket.convoy`: the row drives captain staffing, the ticket field groups the work.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Convoy {
    pub key: ConvoyKey,
    /// Its short number (`#2`), in the order convoys were opened and never reused: a name to say
    /// in place of its epic key (fleetc takes `2` or `#2` wherever a convoy goes).
    #[serde(default)]
    pub no: i64,
    pub title: Option<String>,
    pub created_at: Timestamp,
    pub ended_at: Option<Timestamp>,    // None = active
    pub anchored_at: Option<Timestamp>, // Some = operator-anchored (captain held off relaunch)
}

/// A cog's work-packet — the composed read `fleetc packet` serves: the dispatch order, the
/// ship + its latest snapshot, the cached ticket, and `open_blockers` — the ticket's blockers not
/// yet Done/Cancelled, which gate the merge (`ticket.blocked_by` lists every edge, landed ones
/// too). A sailor reads this once at boot to decide its sweep without re-assembling state from
/// glab/jira. Each piece is optional.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Packet {
    pub order: Option<Order>,
    pub ship: Option<Ship>,
    pub snapshot: Option<ShipSnapshot>,
    pub ticket: Option<Ticket>,
    #[serde(default)]
    pub open_blockers: Vec<TicketKey>,
}

/// fleetd's dispatch verdict for a frontier (To Do) ticket — the absorbed `classify.py`. `None` on
/// a `ConvoyTicketView` means the ticket is past the frontier (not To Do → no dispatch decision).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Classification {
    Clean,   // every blocker terminal → dispatch, fork master
    Stacked, // one in-flight parent (an open MR to fork from), one level deep → dispatch stacked (#8)
    Blocked, // a dependency isn't ready (non-terminal, not a single forkable parent) → wait
    Hold,    // held off the frontier (the captain's groom-gate, or the operator)
}

/// A captain's convoy-scoped view — the composed read `fleetc context --convoy` serves. Each ticket
/// carries its `classification` (fleetd's dispatch verdict — the absorbed candidate+classify) +
/// `dispatched` (an order exists) so the captain dispatches `Clean && !dispatched`, holds what fails
/// its groom-gate, and closes the convoy when every ticket is terminal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConvoyView {
    pub convoy: ConvoyKey,
    /// The convoy's plan — what ties its tickets together (`convoy open --plan`); `None`: no plan.
    #[serde(default)]
    pub plan: Option<String>,
    pub tickets: Vec<ConvoyTicketView>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConvoyTicketView {
    pub key: TicketKey,
    pub status: Option<TicketStatus>,
    pub classification: Option<Classification>,
    pub dispatched: bool,
    pub live_cog: Option<CrewKind>,
    pub ship_state: Option<ShipState>,
    /// For a STACKED ticket: the ref to fork it off — its in-flight parent's branch (`origin/<branch>`).
    pub fork_from: Option<String>,
    /// The blockers not yet Done/Cancelled — what the ticket (and its ship) waits on.
    pub waiting_on: Vec<TicketKey>,
    /// Why it's held, when it is (the hold's reason; `""` when none was given).
    pub held: Option<String>,
    /// The operator works it by hand (`convoy add --hand`): dispatch it to a hand, not a worker.
    #[serde(default)]
    pub hand: bool,
    /// Its worker or sailor escalated, and no verdict judges that escalation yet: /fleet-inspect it.
    #[serde(default)]
    pub inspect: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Approvals { pub current: i64, pub required: i64 }

/// An MR — one of our ships or an ally's (the armada). Stable facts only.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Ship {
    pub id: ShipId,
    pub ticket: Option<TicketKey>, // None for allied ships
    pub iid: i64,                  // the forge's MR number
    pub url: String,
    pub target: String,
    /// The MR's head (source) branch — what a stacked child forks off.
    pub source: String,
    pub pilot: String,             // author — ours vs allied differentiator
    pub created_at: Timestamp,
    pub ended_at: Option<Timestamp>,
    /// The ship-snapshot fingerprint the last sailor was dispatched against (the cold-loop
    /// change-detector). `None` = never dispatched → the first sweep always fires.
    pub swept: Option<String>,
    /// The `head_sha` the last lookout reviewed (the review cold-loop change-detector).
    /// `None` = never reviewed → the first review always fires.
    pub reviewed: Option<String>,
    /// Sailor sweeps launched on it — a review round each. At 10 its sailor escalates.
    #[serde(default)]
    pub rounds: i64,
}

/// The churny forge mirror (1:1 with a `Ship`). Re-synced; stale past `synced_at`.
/// Points one-way at the ship; `Ship` knows nothing about it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShipSnapshot {
    pub ship: ShipId,
    pub title: String,
    pub state: ShipState,
    pub head_sha: String,
    pub has_conflicts: bool,
    pub open_threads: i64,
    /// Every review thread, resolved or not (`open_threads` of them still open).
    #[serde(default)]
    pub total_threads: i64,
    pub approvals: Approvals,
    pub armed: bool,
    pub on_merge_train: bool,
    pub pipeline: Option<PipelineStatus>,
    /// The MRs declared as blocking it that haven't merged (`sync_blocks` keeps them in step).
    #[serde(default)]
    pub blocked_by: Vec<i64>,
    /// GitLab's authoritative merge-readiness (`detailed_merge_status`): `mergeable`,
    /// `conflict`, `need_rebase`, `ci_must_pass`, `discussions_not_resolved`, `not_approved`, …
    /// An OPEN, GitLab-evolving vocabulary (unlike the closed `state`/`pipeline` sets), so it's a
    /// pass-through String, not a CHECK'd enum — the sailor matches the handful it acts on. The
    /// single "why can't this merge" signal that drives the land decision. `None` = not synced.
    pub merge_status: Option<String>,
    pub synced_at: Timestamp,
}

/// Read-layer view-model — the ONLY place `Ship` + snapshot couple. Never a table. `fleetc ship`
/// returns it, with the ref `git fetch origin <fetch_ref>` gets the MR's head by (any author's MR).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShipView {
    pub ship: Ship,
    pub snapshot: Option<ShipSnapshot>,
    pub fetch_ref: String,
    /// The fleet dispatched its ticket (an order): one of our MRs, not an ally's.
    #[serde(default)]
    pub ours: bool,
}

/// `fleetc crossings` — what a lookout's crossing check holds an MR against: the paths it changes,
/// and each of our open tickets beside it (in an active convoy, or carried by one of our MRs), with
/// the ticket's text and what its own MR, if any, shares with this one. Read-layer, never a table.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Crossings {
    pub iid: i64,
    pub title: String,
    pub paths: Vec<String>,
    pub tickets: Vec<Crossing>,
}

/// One of our open tickets, beside the MR under review.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Crossing {
    pub key: TicketKey,
    pub title: String,
    pub status: Option<TicketStatus>,
    pub convoy: Option<ConvoyKey>,
    /// Its open MR, if any.
    pub iid: Option<i64>,
    pub branch: Option<String>,
    /// The paths both MRs change.
    pub shared_paths: Vec<String>,
    /// The folders both MRs change files in (a file's folder, to three levels), beyond the shared
    /// paths: the same crate or module, touched apart.
    pub shared_folders: Vec<String>,
    /// The ticket's text as the tracker holds it (its goal and acceptance criteria). Left out of a
    /// brief answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

/// A ship as the **forge** reports it (a `Forge` port DTO) — facts + mirror fields
/// together, *before* we assign our internal `ShipId`. The sync splits it into a
/// `Ship` (facts) + a `ShipSnapshot` (mirror), assigning/looking up the id by `iid`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ForgeShip {
    pub iid: i64,
    pub url: String,
    pub target: String,
    pub source: String,
    pub pilot: String,
    pub ticket: Option<TicketKey>,
    pub title: String,
    pub state: ShipState,
    pub head_sha: String,
    pub has_conflicts: bool,
    pub open_threads: i64,
    /// Every review thread, resolved or not (`open_threads` of them still open).
    #[serde(default)]
    pub total_threads: i64,
    pub approvals: Approvals,
    pub armed: bool,
    pub on_merge_train: bool,
    pub pipeline: Option<PipelineStatus>,
    /// The MRs declared as blocking it that haven't merged (`sync_blocks` keeps them in step).
    #[serde(default)]
    pub blocked_by: Vec<i64>,
    /// GitLab's `detailed_merge_status` — the merge-readiness signal (see `ShipSnapshot::merge_status`).
    pub merge_status: Option<String>,
}

/// An MR's review surface, computed fresh from the discussions endpoint (a `Forge` port
/// DTO). Two complementary filters over the same fetch: `open_threads` = the merge-blocking
/// count (notes resolvable AND not resolved AND not system); `floating_notes` = the
/// non-resolvable, non-system notes the count excludes (a reviewer HOLD / summary that gates
/// approval, not merge). Replaces the bash `mr-open-threads` + `mr-floating-notes` pair.
/// A new MR as the fleet asks the forge to open it (`Forge::open_mr`). The forge adapter maps it
/// onto its own CLI; squash is the fleet's policy (set here on GitLab, at merge on GitHub).
#[derive(Debug, Clone, PartialEq)]
pub struct MrDraft {
    pub source: String,
    pub target: String,
    pub title: String,
    pub description: String,
    pub draft: bool,
}

/// An MR's CI, as `fleetc ci` reports it: the overall status and each failed job with the tail of
/// its log — what fleet-repair classifies.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CiReport {
    pub status: Option<PipelineStatus>,
    pub failed: Vec<FailedJob>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FailedJob {
    pub name: String,
    pub url: String,
    /// What diagnoses the job: its log's failure lines, then its tail (ANSI colour stripped).
    pub log_excerpt: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReviewState {
    pub open_threads: i64,
    /// Every review thread, resolved or not (`open_threads` of them still open).
    #[serde(default)]
    pub total_threads: i64,
    /// The open (unresolved, merge-blocking) threads themselves — what fleet-parley answers.
    #[serde(default)]
    pub threads: Vec<ReviewThread>,
    pub floating_notes: Vec<FloatingNote>,
}

/// One open review thread: where it points and what its opening comment says.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReviewThread {
    /// The forge's thread id (GitLab discussion id, GitHub review-thread node id) — what
    /// `fleetc reply --thread` takes.
    pub id: String,
    pub path: Option<String>,
    pub line: Option<i64>,
    pub author: Option<String>,
    pub body: String,
}

/// One floating review note — content only; detection here, judgment at the caller.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FloatingNote {
    pub id: i64,
    pub author: Option<String>,
    pub created_at: String,
    pub body: String,
}

/// A ticket as the **tracker** reports it (a `Tracker` port DTO) — the Jira-sourced
/// fields only. The sync upserts these into `tickets` while PRESERVING our own
/// `held` + `created_at` (those are the factory's, not the tracker's).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrackerTicket {
    pub key: TicketKey,
    pub title: String,
    pub status: TicketStatus,
    pub assignee: Option<String>,
    pub convoy: Option<ConvoyKey>,
    pub blocked_by: Vec<TicketKey>,
    #[serde(default)]
    pub labels: Vec<String>,
}

impl TrackerTicket {
    pub fn has(&self, label: TicketLabel) -> bool {
        self.labels.iter().any(|l| l == label.as_str())
    }
}

/// A tracker label the line reads — one meaning, whatever the tracker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TicketLabel {
    /// Filed for a proposal, until the operator accepts it.
    Proposed,
    /// A question for the product owner: a hand works it, never a worker.
    ProductOwner,
}

impl TicketLabel {
    pub fn as_str(self) -> &'static str {
        match self {
            TicketLabel::Proposed => "fleet-proposed",
            TicketLabel::ProductOwner => "product-owner",
        }
    }
}

/// A link between two tickets, as `fleetc ticket block|relate` writes it. `BlockedBy` gates the line
/// (dispatch, merge); `Relates` only points one at the other, both ways, and the fleet never acts on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TicketLink {
    BlockedBy,
    Relates,
}

/// A ticket's full text, as `fleetc ticket <KEY>` shows it: the board facts plus its type
/// (`story`, `bug`, `task`, `epic`, …) and its description.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TicketDetail {
    pub ticket: TrackerTicket,
    pub kind: String,
    pub description: String,
    /// The tickets it relates to (`TicketLink::Relates`): shown, never acted on.
    #[serde(default)]
    pub related: Vec<TicketKey>,
    /// The fleet's hold on it, from the ledger (not the tracker): the reason, `""` when none was
    /// given; `None` when it isn't held.
    #[serde(default)]
    pub held: Option<String>,
    /// Where a person opens it: a URL (Jira) or a file path (the filesystem tracker).
    #[serde(default)]
    pub location: String,
}

/// A new ticket, as `fleetc ticket new` asks the tracker to create it. It starts in To Do.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TicketDraft {
    pub title: String,
    pub kind: String,
    pub description: String,
    pub convoy: Option<ConvoyKey>,
    pub blocked_by: Vec<TicketKey>,
    #[serde(default)]
    pub labels: Vec<String>,
}

/// Work a cog found mid-ticket that isn't its own, held for the operator (or the admiral) to accept
/// or drop. A new ticket is filed when proposed, labelled `fleet-proposed` and outside any convoy, and
/// held as an existing one; accept makes it official, drop cancels it. Open while `ended_at` is
/// `None`; `accepted_as` names the ticket it is.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Proposal {
    pub id: ProposalId,
    /// The ticket the proposing cog was working (a new ticket's default convoy).
    pub from_ticket: Option<TicketKey>,
    pub proposed: Proposed,
    pub created_at: Timestamp,
    pub ended_at: Option<Timestamp>,
    pub accepted_as: Option<TicketKey>,
}

/// What a proposal asks for: a new ticket, drafted to file; or an existing one to take on, and why.
/// Either way it carries the convoy it lands in on accept — the proposing ticket's, unless named.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Proposed {
    New(TicketDraft),
    Existing { ticket: TicketKey, why: String, convoy: Option<ConvoyKey> },
}

impl Proposed {
    /// The convoy it lands in on accept: a new ticket is filed into it, an existing one joins it.
    pub fn convoy(&self) -> Option<&ConvoyKey> {
        match self {
            Proposed::New(d) => d.convoy.as_ref(),
            Proposed::Existing { convoy, .. } => convoy.as_ref(),
        }
    }
    fn convoy_mut(&mut self) -> &mut Option<ConvoyKey> {
        match self {
            Proposed::New(d) => &mut d.convoy,
            Proposed::Existing { convoy, .. } => convoy,
        }
    }
    /// Land it in `convoy` when given (the accepter's pick), else keep its own.
    pub fn override_convoy(&mut self, convoy: Option<ConvoyKey>) {
        if convoy.is_some() {
            *self.convoy_mut() = convoy;
        }
    }
}

/// A verdict on a cog's escalation (/fleet-inspect): CONFIRM — it stands, the operator's to
/// rule; REFUTE — its way out was in reach, and was sent to the cog.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Confirm,
    Refute,
}

/// One inspection of a cog's escalation: the case it judged (and the report that stood then), the
/// verdict, and the verdict's text (/fleet-inspect's template). The latest per case counts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Inspection {
    pub session: SessionId,
    /// The case it judged. `None` only for a verdict older than cases that no case took in.
    pub case: Option<CaseId>,
    pub reported_at: Timestamp,
    pub verdict: Verdict,
    pub body: String,
    pub inspected_at: Timestamp,
}

impl Inspection {
    /// This verdict judges that case: the escalation as it stands, not an earlier one.
    pub fn judges(&self, c: &Case) -> bool {
        self.case.as_ref() == Some(&c.id)
    }

    /// The verdict's headline, the template's first line without its label.
    pub fn headline(&self) -> &str {
        self.line("**Verdict:**").unwrap_or(self.verdict.as_str())
    }

    /// The verdict's recommendation: a REFUTE's way out, a CONFIRM's "operator's ruling".
    pub fn recommendation(&self) -> Option<&str> {
        self.line("- **Recommendation:**")
    }

    /// A CONFIRM's prepared part: the in-lane steps that go to the cog with the ruling. `None` when
    /// the verdict has none ("none", or no such line). Its continuation lines (indented) included.
    pub fn prepared(&self) -> Option<String> {
        let mut lines = self.body.lines().skip_while(|l| !l.trim_start().starts_with("- **Prepared:**"));
        let first = lines.next()?.trim_start().strip_prefix("- **Prepared:**")?.trim();
        let rest = lines.take_while(|l| l.starts_with(char::is_whitespace) && !l.trim().is_empty());
        let text = std::iter::once(first).chain(rest.map(str::trim)).filter(|l| !l.is_empty()).collect::<Vec<_>>().join("\n");
        (!text.is_empty() && !text.eq_ignore_ascii_case("none")).then_some(text)
    }

    fn line(&self, label: &str) -> Option<&str> {
        self.body.lines().find_map(|l| l.trim_start().strip_prefix(label.trim_start())).map(str::trim)
    }
}

/// An escalation, start to finish: a worker's or sailor's escalation (raised), the lookout's
/// verdict on it (inspected), and the operator's ruling (ruled). Until it's answered (a REFUTE's
/// way out, or a ruling), another escalation from its cog joins it, whatever its wording, so it
/// isn't judged twice; once answered, another escalation is a new case. It settles when its cog
/// reports anything else. A ruling made with no escalation behind it is a case with no cog, settled at
/// once. The cases of a convoy are its precedent: a cog reads them before it escalates.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Case {
    pub id: CaseId,
    pub ticket: TicketKey,
    /// The ticket's convoy when the case was raised: whose cases it is among.
    pub convoy: Option<ConvoyKey>,
    /// The escalated cog's session and kind; `None` for a ruling with no escalation.
    pub session: Option<SessionId>,
    pub cog: Option<CrewKind>,
    /// What the cog escalated (its report's `last_action`), or the ruling's question.
    pub question: String,
    pub raised_at: Timestamp,
    /// The reports that raised it: one sent while it awaited its answer counts here.
    pub reports: i64,
    pub ruling: Option<Ruling>,
    pub settled_at: Option<Timestamp>,
}

/// The operator's answer to a case: the question in general terms, so a later case can cite it,
/// and the answer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Ruling {
    pub question: String,
    pub answer: String,
    pub ruled_at: Timestamp,
}

impl Case {
    /// Where the case stands, given its latest verdict: `open` (not inspected yet), `refuted` (its
    /// way out went to the cog), `confirmed` (awaiting a ruling), `ruled`, `unruled` (its cog moved
    /// on with no ruling recorded), or `withdrawn` (its cog moved on before an inspection).
    pub fn outcome(&self, verdict: Option<Verdict>) -> &'static str {
        match (self.ruling.is_some(), verdict, self.settled_at.is_some()) {
            (true, _, _) => "ruled",
            (false, Some(Verdict::Refute), _) => "refuted",
            (false, Some(Verdict::Confirm), false) => "confirmed",
            (false, Some(Verdict::Confirm), true) => "unruled",
            (false, None, false) => "open",
            (false, None, true) => "withdrawn",
        }
    }
}

/// The CDC change-log entry (PLAN §3.1 `events`). Appended in the same txn as the
/// mutation it records. `TickRan` events carry the version-span in `payload`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub seq: Seq,
    pub created_at: Timestamp,
    pub kind: EventKind,
    pub entity_kind: EntityKind,
    pub entity_ref: String, // stringified id/key (polymorphic)
    pub payload: serde_json::Value,
}

/// A dispatch order — the queue (operational intent). Its builder is due until a cog starts on
/// the ticket (`builder_due`); `auto_impl` defaults true.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Order {
    pub ticket: TicketKey,
    pub auto_impl: bool,   // true = worker · false = hand
    pub fork_from: String,
    pub ac_summary: String,
    pub park_at: Option<String>,
    /// Worktree the crew runs in. Materialised by the producer at dispatch (like the
    /// captain's /fleet-berth); reconcile reads it at spawn and never creates it.
    pub worktree_path: Option<String>,
    pub created_at: Timestamp,
}

impl Order {
    /// A builder is due: no cog among the ticket's `sessions` started since the order was
    /// dispatched (or re-dispatched by `fleetc rework`) — so the ticket waits for one, whatever
    /// MR it already has.
    pub fn builder_due(&self, sessions: &[Session]) -> bool {
        let at = |t: &str| time::OffsetDateTime::parse(t, &time::format_description::well_known::Rfc3339).ok();
        let ordered = at(&self.created_at);
        sessions.iter().all(|s| match (at(&s.created_at), ordered) {
            (Some(started), Some(ordered)) => started < ordered,
            _ => s.created_at < self.created_at, // unparseable (test fixtures): compare as text
        })
    }
}

// ── SQL string mapping for the CHECK-constrained enums ──────────────────────────
// Pure string forms (match the serde snake_case). The `ledger` adapter uses these
// to map enum ↔ TEXT; `domain` stays free of any sqlite dependency.

impl Verdict {
    pub fn as_str(&self) -> &'static str {
        match self {
            Verdict::Confirm => "confirm",
            Verdict::Refute => "refute",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "confirm" => Some(Self::Confirm),
            "refute" => Some(Self::Refute),
            _ => None,
        }
    }
}

impl CrewKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            CrewKind::Worker => "worker",
            CrewKind::Sailor => "sailor",
            CrewKind::Hand => "hand",
            CrewKind::Captain => "captain",
            CrewKind::Lookout => "lookout",
            CrewKind::Admiral => "admiral",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "worker" => Some(Self::Worker),
            "sailor" => Some(Self::Sailor),
            "hand" => Some(Self::Hand),
            "captain" => Some(Self::Captain),
            "lookout" => Some(Self::Lookout),
            "admiral" => Some(Self::Admiral),
            _ => None,
        }
    }
}

impl WorkerState {
    pub fn as_str(&self) -> &'static str {
        match self {
            WorkerState::Started => "started",
            WorkerState::Implementing => "implementing",
            WorkerState::Review => "review",
            WorkerState::Waiting => "waiting",
            WorkerState::Concluded => "concluded",
            WorkerState::Escalated => "escalated",
            WorkerState::Anchored => "anchored",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "started" => Some(Self::Started),
            "implementing" => Some(Self::Implementing),
            "review" => Some(Self::Review),
            "waiting" => Some(Self::Waiting),
            "concluded" => Some(Self::Concluded),
            "escalated" => Some(Self::Escalated),
            "anchored" => Some(Self::Anchored),
            _ => None,
        }
    }
}

impl SailorState {
    pub fn as_str(&self) -> &'static str {
        match self {
            SailorState::Sleeping => "sleeping",
            SailorState::Escalated => "escalated",
            SailorState::Anchored => "anchored",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "sleeping" => Some(Self::Sleeping),
            "escalated" => Some(Self::Escalated),
            "anchored" => Some(Self::Anchored),
            _ => None,
        }
    }
}

impl HandState {
    pub fn as_str(&self) -> &'static str {
        match self {
            HandState::Awaiting => "awaiting",
            HandState::Implementing => "implementing",
            HandState::Concluded => "concluded",
            HandState::Done => "done",
            HandState::Anchored => "anchored",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "awaiting" => Some(Self::Awaiting),
            "implementing" => Some(Self::Implementing),
            "concluded" => Some(Self::Concluded),
            "done" => Some(Self::Done),
            "anchored" => Some(Self::Anchored),
            _ => None,
        }
    }
}

impl LookoutState {
    pub fn as_str(&self) -> &'static str {
        match self {
            LookoutState::Reviewed => "reviewed",
            LookoutState::Escalated => "escalated",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "reviewed" => Some(Self::Reviewed),
            "escalated" => Some(Self::Escalated),
            _ => None,
        }
    }
}

impl CaptainState {
    pub fn as_str(&self) -> &'static str {
        match self {
            CaptainState::Done => "done",
            CaptainState::Escalated => "escalated",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "done" => Some(Self::Done),
            "escalated" => Some(Self::Escalated),
            _ => None,
        }
    }
}

impl CogState {
    /// Which cog this state belongs to (the enum variant carries it).
    pub fn cog(&self) -> CrewKind {
        match self {
            CogState::Worker(_) => CrewKind::Worker,
            CogState::Sailor(_) => CrewKind::Sailor,
            CogState::Hand(_) => CrewKind::Hand,
            CogState::Captain(_) => CrewKind::Captain,
            CogState::Lookout(_) => CrewKind::Lookout,
        }
    }
    pub fn as_str(&self) -> &'static str {
        match self {
            CogState::Worker(s) => s.as_str(),
            CogState::Sailor(s) => s.as_str(),
            CogState::Hand(s) => s.as_str(),
            CogState::Captain(s) => s.as_str(),
            CogState::Lookout(s) => s.as_str(),
        }
    }
    /// Widen a stored string back to a typed state, given the session's cog. The cog
    /// selects the vocabulary — the same string can be valid for one cog, not another.
    pub fn parse(cog: CrewKind, s: &str) -> Option<Self> {
        match cog {
            CrewKind::Worker => WorkerState::parse(s).map(CogState::Worker),
            CrewKind::Sailor => SailorState::parse(s).map(CogState::Sailor),
            CrewKind::Hand => HandState::parse(s).map(CogState::Hand),
            CrewKind::Captain => CaptainState::parse(s).map(CogState::Captain),
            CrewKind::Lookout => LookoutState::parse(s).map(CogState::Lookout),
            CrewKind::Admiral => None, // an admiral reports nothing: the cog's own reports end it
        }
    }
    /// A builder's happy terminal — MR open + reviewed, ready to hand to a sailor.
    pub fn is_concluded(&self) -> bool {
        matches!(self, CogState::Worker(WorkerState::Concluded) | CogState::Hand(HandState::Concluded))
    }
    /// A cold cog's clean finish — a sailor swept (`sleeping`), a captain `done`, the lookout
    /// `reviewed` — now idling. The self-reported counterpart to a Dead process: reconcile stands it
    /// down (graceful reap) so its trigger re-launches on the next change. `claude --agent` does not self-exit,
    /// so this report — not a dead tmux — is what signals the clean finish.
    pub fn is_done(&self) -> bool {
        matches!(
            self,
            CogState::Sailor(SailorState::Sleeping)
                | CogState::Captain(CaptainState::Done)
                | CogState::Lookout(LookoutState::Reviewed)
        )
    }
    /// The cog escalated: it stopped for a ruling it can't make.
    pub fn is_escalated(&self) -> bool {
        matches!(
            self,
            CogState::Worker(WorkerState::Escalated)
                | CogState::Sailor(SailorState::Escalated)
                | CogState::Captain(CaptainState::Escalated)
                | CogState::Lookout(LookoutState::Escalated)
        )
    }
    /// A DELIBERATE non-terminal hold (a wait on blockers or on a review, an anchor).
    /// reconcile must not heal or stand down a cog reporting this even when its liveness is Dead —
    /// it stopped on purpose. An escalation isn't one: a dead escalated cog is healed in place.
    pub fn is_paused(&self) -> bool {
        matches!(
            self,
            CogState::Worker(WorkerState::Review | WorkerState::Waiting | WorkerState::Anchored)
                | CogState::Sailor(SailorState::Anchored)
                | CogState::Hand(HandState::Anchored)
        )
    }
}

/// Serialize as the bare string (`"concluded"`) for the context/event JSON — the
/// cog is already implied by the session the report hangs off.
impl Serialize for CogState {
    fn serialize<S: serde::Serializer>(&self, ser: S) -> std::result::Result<S::Ok, S::Error> {
        ser.serialize_str(self.as_str())
    }
}

impl TicketStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            TicketStatus::Todo => "todo",
            TicketStatus::Blocked => "blocked",
            TicketStatus::InProgress => "in_progress",
            TicketStatus::InQa => "in_qa",
            TicketStatus::Done => "done",
            TicketStatus::Cancelled => "cancelled",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "todo" => Some(Self::Todo),
            "blocked" => Some(Self::Blocked),
            "in_progress" => Some(Self::InProgress),
            "in_qa" => Some(Self::InQa),
            "done" => Some(Self::Done),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }
}

impl ShipState {
    pub fn as_str(&self) -> &'static str {
        match self {
            ShipState::Draft => "draft",
            ShipState::Open => "open",
            ShipState::Merged => "merged",
            ShipState::Closed => "closed",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "draft" => Some(Self::Draft),
            "open" => Some(Self::Open),
            "merged" => Some(Self::Merged),
            "closed" => Some(Self::Closed),
            _ => None,
        }
    }
}

impl PipelineStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            PipelineStatus::Running => "running",
            PipelineStatus::Success => "success",
            PipelineStatus::Failed => "failed",
            PipelineStatus::Canceled => "canceled",
            PipelineStatus::Manual => "manual",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "running" => Some(Self::Running),
            "success" => Some(Self::Success),
            "failed" => Some(Self::Failed),
            "canceled" => Some(Self::Canceled),
            "manual" => Some(Self::Manual),
            _ => None,
        }
    }
}

impl EntityKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            EntityKind::Session => "session",
            EntityKind::Ticket => "ticket",
            EntityKind::Ship => "ship",
            EntityKind::Escalation => "escalation",
            EntityKind::Ticker => "ticker",
            EntityKind::Convoy => "convoy",
            EntityKind::Lookout => "lookout",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "session" => Some(Self::Session),
            "ticket" => Some(Self::Ticket),
            "ship" => Some(Self::Ship),
            "escalation" => Some(Self::Escalation),
            "ticker" => Some(Self::Ticker),
            "convoy" => Some(Self::Convoy),
            "lookout" => Some(Self::Lookout),
            _ => None,
        }
    }
}

/// Default liveness thresholds (seconds). The *defaults*; the running values are
/// injected from config (fleetd), so they change without a recompile.
pub const LIVENESS_ACTIVE_SECS: i64 = 120;
pub const LIVENESS_QUIET_SECS: i64 = 1800;

/// Liveness thresholds — read-time policy, injected from config. `Default` = the
/// built-in constants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LivenessThresholds {
    pub active_secs: i64,
    pub quiet_secs: i64,
}

impl Default for LivenessThresholds {
    fn default() -> Self {
        Self { active_secs: LIVENESS_ACTIVE_SECS, quiet_secs: LIVENESS_QUIET_SECS }
    }
}

impl Liveness {
    /// Derive liveness from whether the session's host process is alive and how many
    /// seconds since its last observed activity, against injected thresholds. Read-time
    /// only — never stored.
    pub fn classify(alive: bool, age_secs: Option<i64>, t: &LivenessThresholds) -> Liveness {
        if !alive {
            return Liveness::Dead;
        }
        match age_secs {
            Some(a) if a < t.active_secs => Liveness::Active,
            Some(a) if a < t.quiet_secs => Liveness::Quiet,
            _ => Liveness::Idle,
        }
    }
}

impl EventKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            EventKind::SessionStarted => "session_started",
            EventKind::SessionEnded => "session_ended",
            EventKind::StatusReported => "status_reported",
            EventKind::Dispatched => "dispatched",
            EventKind::TicketCreated => "ticket_created",
            EventKind::TicketSynced => "ticket_synced",
            EventKind::TicketBlocked => "ticket_blocked",
            EventKind::TicketUnblocked => "ticket_unblocked",
            EventKind::TicketHeld => "ticket_held",
            EventKind::TicketReleased => "ticket_released",
            EventKind::TicketDecommissioned => "ticket_decommissioned",
            EventKind::TicketRecalled => "ticket_recalled",
            EventKind::TicketAmended => "ticket_amended",
            EventKind::ShipOpened => "ship_opened",
            EventKind::ShipSynced => "ship_synced",
            EventKind::ShipMerged => "ship_merged",
            EventKind::ShipClosed => "ship_closed",
            EventKind::EscalationInspected => "escalation_inspected",
            EventKind::EscalationRuled => "escalation_ruled",
            EventKind::ConvoyOpened => "convoy_opened",
            EventKind::ConvoyClosed => "convoy_closed",
            EventKind::ConvoyPlanned => "convoy_planned",
            EventKind::SessionAnchored => "session_anchored",
            EventKind::SessionWeighed => "session_weighed",
            EventKind::ConvoyAnchored => "convoy_anchored",
            EventKind::ConvoyWeighed => "convoy_weighed",
            EventKind::LookoutAnchored => "lookout_anchored",
            EventKind::LookoutWeighed => "lookout_weighed",
            EventKind::TickRan => "tick_ran",
            EventKind::HealthChecked => "health_checked",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "session_started" => Some(Self::SessionStarted),
            "session_ended" => Some(Self::SessionEnded),
            "status_reported" => Some(Self::StatusReported),
            "dispatched" => Some(Self::Dispatched),
            "ticket_created" => Some(Self::TicketCreated),
            "ticket_synced" => Some(Self::TicketSynced),
            "ticket_blocked" => Some(Self::TicketBlocked),
            "ticket_unblocked" => Some(Self::TicketUnblocked),
            "ticket_held" => Some(Self::TicketHeld),
            "ticket_released" => Some(Self::TicketReleased),
            "ticket_decommissioned" => Some(Self::TicketDecommissioned),
            "ticket_recalled" => Some(Self::TicketRecalled),
            "ticket_amended" => Some(Self::TicketAmended),
            "ship_opened" => Some(Self::ShipOpened),
            "ship_synced" => Some(Self::ShipSynced),
            "ship_merged" => Some(Self::ShipMerged),
            "ship_closed" => Some(Self::ShipClosed),
            "escalation_inspected" => Some(Self::EscalationInspected),
            "escalation_ruled" => Some(Self::EscalationRuled),
            "convoy_opened" => Some(Self::ConvoyOpened),
            "convoy_closed" => Some(Self::ConvoyClosed),
            "convoy_planned" => Some(Self::ConvoyPlanned),
            "session_anchored" => Some(Self::SessionAnchored),
            "session_weighed" => Some(Self::SessionWeighed),
            "convoy_anchored" => Some(Self::ConvoyAnchored),
            "convoy_weighed" => Some(Self::ConvoyWeighed),
            "lookout_anchored" => Some(Self::LookoutAnchored),
            "lookout_weighed" => Some(Self::LookoutWeighed),
            "tick_ran" => Some(Self::TickRan),
            "health_checked" => Some(Self::HealthChecked),
            _ => None,
        }
    }
}

#[cfg(test)]
mod liveness_tests {
    use super::*;

    #[test]
    fn classify_respects_injected_thresholds() {
        let t = LivenessThresholds { active_secs: 10, quiet_secs: 60 };
        assert_eq!(Liveness::classify(false, None, &t), Liveness::Dead);
        assert_eq!(Liveness::classify(true, Some(5), &t), Liveness::Active);
        assert_eq!(Liveness::classify(true, Some(30), &t), Liveness::Quiet);
        assert_eq!(Liveness::classify(true, Some(120), &t), Liveness::Idle);
        assert_eq!(Liveness::classify(true, None, &t), Liveness::Idle);
    }

    #[test]
    fn ticket_key_parse_accepts_only_the_prefix_dash_number_shape() {
        for ok in ["PROJ-1", "PROJ-6127", "T-12", "AB2-7"] {
            assert!(TicketKey::parse(ok).is_ok(), "{ok}");
        }
        for bad in ["proj-1", "PROJ-", "-1", "PROJ1", "PROJ-1a", "2AB-1", "PROJ-1-2", "", "--flag"] {
            assert!(TicketKey::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn convoy_key_shares_the_ticket_key_shape() {
        assert!(ConvoyKey::parse("PROJ-100").is_ok());
        assert!(ConvoyKey::parse("forge-work").is_err());
    }

    #[test]
    fn ticket_key_find_in_scans_for_the_prefix_on_a_word_boundary() {
        let k = |s: &str| TicketKey::find_in(s, "PROJ").map(|t| t.0);
        assert_eq!(k("feature/PROJ-12-add-forge").as_deref(), Some("PROJ-12"));
        assert_eq!(k("Bump SHA-256 (PROJ-7)").as_deref(), Some("PROJ-7"));
        assert_eq!(k("XPROJ-3 then PROJ-4").as_deref(), Some("PROJ-4"), "word boundary");
        assert_eq!(k("PROJ-x"), None);
        assert_eq!(k("Fix UTF-8 decoding"), None, "other prefixes never match");
    }

    #[test]
    fn transcript_encodes_every_non_alphanumeric_char_of_the_cwd() {
        // A non-existent worktree → canonicalize falls back to the raw path.
        let s = Session {
            id: SessionId(1),
            kind: CrewKind::Hand,
            ticket: None,
            convoy: None,
            worktree_path: "/wt/feature-PROJ-1.Some_Thing x".into(),
            tmux_session: "hand".into(),
            harness_session_id: Some("abc-123".into()),
            created_at: "t".into(),
            ended_at: None,
            anchored_at: None,
            ship: None,
        };
        let home = std::env::var("HOME").unwrap();
        let want = format!("{home}/.claude/projects/-wt-feature-PROJ-1-Some-Thing-x/abc-123.jsonl");
        assert_eq!(s.transcript().unwrap().to_string_lossy(), want);
        assert_eq!(Session { harness_session_id: None, ..s }.transcript(), None, "pre-capture: no id");
    }
}

#[cfg(test)]
mod order_tests {
    use super::*;

    #[test]
    fn a_builder_is_due_until_a_cog_starts_after_the_order() {
        let order = |at: &str| Order {
            ticket: TicketKey("PROJ-1".into()),
            auto_impl: true,
            fork_from: "origin/main".into(),
            ac_summary: "s".into(),
            park_at: None,
            worktree_path: Some("/wt".into()),
            created_at: at.into(),
        };
        let session = |at: &str| Session {
            id: SessionId(1),
            kind: CrewKind::Sailor,
            ticket: Some(TicketKey("PROJ-1".into())),
            convoy: None,
            worktree_path: "/wt".into(),
            tmux_session: "sailor-PROJ-1".into(),
            harness_session_id: None,
            created_at: at.into(),
            ended_at: None,
            anchored_at: None,
            ship: None,
        };
        let sailed = [session("2026-10-28T10:00:00.5Z")];
        assert!(order("2026-10-28T10:00:00Z").builder_due(&[]), "dispatched, nothing launched yet");
        assert!(!order("2026-10-28T10:00:00Z").builder_due(&sailed), "a cog started after it");
        // Reworked: re-dated past every session. Compared as times — as text, ".5Z" sorts after ".52Z".
        assert!(order("2026-10-28T10:00:00.52Z").builder_due(&sailed));
    }
}

#[cfg(test)]
mod inspection_tests {
    use super::*;

    fn verdict(body: &str) -> Inspection {
        Inspection {
            session: SessionId(1),
            case: Some(CaseId(1)),
            reported_at: "t".into(),
            verdict: Verdict::Confirm,
            body: body.into(),
            inspected_at: "t".into(),
        }
    }

    #[test]
    fn a_confirm_carries_its_prepared_part_and_none_reads_as_none() {
        let v = verdict("**Verdict:** CONFIRM — rule on X\n- **Recommendation:** operator's ruling\n- **Prepared:** answer threads 4 and 7\n  as the canon says (§5)\n- **Other:** x");
        assert_eq!(v.headline(), "CONFIRM — rule on X");
        assert_eq!(v.prepared().as_deref(), Some("answer threads 4 and 7\nas the canon says (§5)"), "with its indented continuation");
        assert_eq!(verdict("- **Prepared:** none").prepared(), None);
        assert_eq!(verdict("**Verdict:** CONFIRM — no prepared line").prepared(), None);
    }
}
