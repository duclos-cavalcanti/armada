# MADY — the programmatic pivot

Turn the factory's substrate from **cron loops + bash scripts + filesystem-state
JSON** into an **always-on program**: a single-writer daemon owning a real
database, a private control channel, and deterministic scheduling — *inspired by*
apiary/`beekeeperd`, but built with our own factory metaphor and our own ideas.

> **This is a living design doc, not a spec.** We grow it turn by turn. The
> mapping (§3) is grounded; everything else is a scaffold to describe into.
> Nothing here is committed to code until an entry graduates to `TODO.md`.
>
> **What we are NOT doing:** re-using apiary, or rewriting the cogs / skills /
> prompts / gate *rules*. Only the **substrate** moves (cron → scheduler,
> files → DB). Cog behavior stays.

---

## 1. Why — the pain this earns its place against

The daemon is *more* machinery, the opposite of our lean, cron-driven default —
so it has to pay rent. The rent it pays: a **single-writer DB with durable-facts-
only + status-derived-at-read** structurally kills the fs-state failure classes
that dominate our open findings:

- arm-state / status **lies** — a stored `state` that no longer matches reality
  (`TODO` #49, #51, #58).
- **races** — two writers on one file: zombie-respawn, partial-reap loops
  (`TODO` #45, #55).
- **silent no-op writes** — a mutation that fails but reports success
  (`TODO` #48, #53).

If those stop being the pain, this plan should stop too. (KISS check, standing.)

---

## 2. Concepts & factory analogies

apiary uses a bee metaphor (queen / workers / hive / `beekeeperd`). We keep the
**factory** metaphor. Names below are **proposals** — pick/replace as we go.

| Concept (what it is) | apiary term | Our factory analogy (proposed) | Status |
|---|---|---|---|
| Always-on deterministic daemon (the spine) | `beekeeperd` | **`fleetd`** — the daemon that owns & paces the fleet of cogs | ✅ locked |
| Durable-facts database (SQLite, WAL) | the DB | **the `ledger`** (`ledger.db`) | ✅ locked |
| The only sanctioned write path (CLI) | `bk` | **`fleetc`** — the fleet client CLI | ✅ locked |
| Private control channel (HTTP-over-UDS) | UDS API | **the control bus** / the wiring | 🔲 undecided |
| Ordered change-log / event stream | `events` NDJSON | **the ticker tape** | 🔲 undecided |
| Live-session tracker (owns tmux, derives liveness) | session registry | **the floor roster** / muster | 🔲 undecided |
| The pacer (fires ticks, replaces cron) | scheduler | **the shift clock** / metronome | 🔲 undecided |
| External-surface drivers (GitLab/Jira) | forge/tracker ports | **adapters** (we already have these) | ✅ keep name |

**Glossary**
- **HTTP-over-UDS** — ordinary HTTP (GET/POST + JSON) spoken over a Unix-socket
  *file*, not a TCP port. Auth = filesystem perms on the socket; no open port;
  SSH-forwardable for a future remote box.
- **Durable facts, derived status** — store only what was *observed* (session
  ended_at, heartbeats, MR sha); compute liveness/state at read time. Never store
  a status that can rot.
- **Single writer** — exactly one process mutates the DB, in transactions →
  no last-writer-wins races.

### 2.1 Repository layout — a Rust workspace under `crates/`

One Cargo **virtual workspace**: the root `Cargo.toml` is `[workspace]`-only (no
root package, no root `src/`), and *every* crate lives under `crates/`, all
sharing one base. No app is privileged over another. Layout is **flat**
(`members = ["crates/*"]`); the lib/app split lives in names + this doc, not the
tree. Revisit grouping (`crates/{libs,apps}/`) only past ~a dozen crates.

**Libraries (the base) — dependencies point *inward*:**
- **`crates/core`** — pure domain: entities + **ports** (repository / session-host /
  forge / tracker traits) + use-cases. No sqlite, no tmux, no http. The source of
  truth for the factory's *shape*.
- **`crates/ledger`** — the **SQLite adapter**: implements the repository port over
  `ledger.db`; holds the DDL + the row↔domain mapping. `fleetd`-only. (Persistence
  is a driven adapter in Hexagonal — hence "ledger" is an adapter, not the core.)
- **`crates/wire`** — the shared **over-the-socket types** + the UDS client
  transport. Both the server (`fleetd`) and every client depend on it.

**Apps:**
- **`crates/fleetd`** — the daemon/server: `core` use-cases wired to `ledger`
  (persistence) + a `tmux` adapter (sessions) + the UDS API. **The only writer.**
- **`crates/fleetc`** — the **client CLI** (the console / sanctioned write path):
  verbs → `wire` → `fleetd`.
- **`crates/crew`** — TUI **read** client.
- **`crates/dashboard`** — TUI **read** client.

**Dependency rule (Hexagonal):** apps depend on `core`'s ports; adapters
(`ledger`, the `wire` client, `tmux`) *implement* those ports; nothing in `core`
knows about sqlite/http/tmux. Swapping persistence = swapping the adapter, never
touching `core`.

> Contrast apiary, whose root *is* a package (`src/main.rs` = the original
> `beekeeper` TUI). We start with a pure virtual workspace so every crate is a peer
> over one `core`.

**crew/dashboard re-adaptation (a later phase).** They're standalone Clean-Arch
crates today (crew already has a `state_store` port + an `fs_store` adapter). The
rewrite gives them, behind the *same* ports, a new **fleet-client adapter** (over
`wire`) while **keeping `fs_store` as the legacy fallback**. Picking the adapter at
startup (`--bk` vs `--files`) *is* the byte-identical dual-mode migration — and it
falls straight out of the architecture we already have.

---

## 3. The mapping — MADY today → the program

**Scope / ambition: `fleetd` absorbs the mechanics — `bin/` largely dissolves.**
The loop ticks, `converge` + the foreman `features/`, the `lib/`, and the `adapters/`
all become Rust *inside* fleetd (behind ports); operator commands become `fleetc`
subcommands. Aspirationally *all* of `bin/` is absorbed; residuals to decide later
(`install.sh`, launchd bring-up). Sub-decision, per-driver: a port may **shell out**
to `glab`/`jira`/`tmux` (fast, reuses what works) **or** use a **native API client**
(cleaner) — either is valid behind the port.

Grounded against the current tree.

| MADY today | Moves to |
|---|---|
| `converge.sh` + `loop/*-tick.sh` (cron) | the daemon's **scheduler/pacer** |
| `~/.mady/state/*.json` (status drops, dispatch/escalation/resolution sidecars, `jira.json`/`mrs.json` caches) | **DB tables** (facts) + a *legacy mirror* of the JSON during cutover |
| write-side of `adapters/{jira,glab}` + `dispatch.sh` / `claim.sh` | **console (CLI) verbs** → daemon → adapters |
| `context.sh` live-session enumeration | the **session registry** |
| `bin/features/agents/foreman/{candidate,classify,epic,queue,dispatch}` logic | daemon **reconcile** behind adapter ports |
| `schemas/*.schema.json` | the **DB schema** + a wire **version header** |
| `dashboard/` + `crew/` (ratatui) | the daemon's **TUI clients** (`--bk` / `--files` dual mode) |
| cron (crontab `mady-*` tags) | **one** launchd/`run.sh`-managed daemon process |

**Stays unchanged:** the cogs (foreman/worker/finisher/inspector/auditor/…), the
`mady-*` skills, agent prompts, dispatch packets & queue boards as *intent files*,
gate *rules* (candidate/classify/epic logic), tmux + cmux as the surface.

---

## 3.1 Entity encodings (`core` ↔ `ledger`)

Decided one entity at a time. **Facts only; status derived.** Crew vocabulary:
agents are the **crew** (README §2), not "cogs". Crew roles = **Worker** (builds
the ship) / **Sailor** (sails it into merge) / **Hand** (the operator's hands).
Inspector is removed (principal-reviewer is the validation stamp).

### `sessions` — the central entity — DECIDED 2026-09-29

Collapses ~330 `workers/`+`finishers/`+`humans/` `.jsonl` drops into rows; a
ticket that went worker→sailor is **two rows, one unit**. The old "fat drop"
fields (`mr`, `escalations`, `state`, `last_action`, `tick_count`) move to their
own entities or are derived.

```rust
// crates/core
pub enum CrewKind { Worker, Sailor, Hand }
pub struct Session {
    id, kind, ticket,            // FK → units; many sessions per unit over time
    worktree_path, tmux_session, // tmux name stored, never synthesized (adoption-safe)
    harness_session_id,          // Option — transcript lookup
    created_at, ended_at,        // ended_at None = live (the only lifecycle FACT)
}
// port: SessionRepo { upsert, mark_ended, sessions_for(ticket), live() }
```
```sql
-- crates/ledger
CREATE TABLE sessions (
    id INTEGER PRIMARY KEY,
    kind TEXT NOT NULL CHECK (kind IN ('worker','sailor','hand')),
    ticket TEXT NOT NULL REFERENCES units(key),
    worktree_path TEXT NOT NULL, tmux_session TEXT NOT NULL,
    harness_session_id TEXT, created_at TEXT NOT NULL, ended_at TEXT
);
CREATE UNIQUE INDEX one_live_crew_per_ticket
    ON sessions(ticket, kind) WHERE ended_at IS NULL;   -- DB-enforced spawn lock
```
- **Cut vs today:** `branch` (derive from worktree), `exit_code` (rarely
  available), and all non-session fields.
- **`ended_at` is the only lifecycle fact;** liveness / staleness / `state` are
  derived at read time.
- **The partial unique index is a structural race-fix** — "one live crew per
  (ticket,kind)" enforced by the DB kills the double-spawn / zombie-respawn / stale
  bugs (#45/#55/#58) by construction.
- **Scope: line crew only** (ticket-bound). The free-roaming hand + standing crew
  (foreman/auditor/…) are a later "ad-hoc sessions" shape (nullable ticket).

### standing rule — no vendor leak in `core`

Entities speak the **domain**; the vendor name (Jira, GitLab) appears **only** in
its adapter/driver, which maps the vendor's shape onto ours. So: `TicketStatus`
not `JiraStatus`, `status`/`synced_at` not `jira_*`, `ship` not `merge_request`.
The `ledger` is *our* persistence too → no `jira_`/`gitlab_` columns there either.

### standing rule — canonical timestamps

Every entity uses the **same** three timestamp names (RFC3339 UTC `TEXT`):
- **`created_at`** (NOT NULL) — birth (when the factory first recorded it).
- **`ended_at`** (NULL = still active) — reached a terminal state.
- **`synced_at`** (NULL = never) — a cached vendor mirror was last refreshed.

Identifiers follow a parallel rule: synthetic integer PK = `id` (`SessionId`,
`ShipId`); meaningful string natural key = `key` (`TicketKey`).

### `ticket` — the work order — DECIDED 2026-09-29

Identity + grooming attributes + a cached tracker mirror. Most of the dispatch
packet is NOT the ticket (that's dispatch intent; `worktree_path` is on `sessions`).

```rust
// crates/core
pub enum TicketStatus { ToDo, InProgress, InQA, Done, Cancelled }  // cached mirror

pub struct Ticket {
    key,                      // PK "MADY-1234"
    epic: Option<TicketKey>,  // grouping; an epic is itself a ticket
    held: bool,               // mady-hold: operator decision pending
    created_at,               // fact (birth)
    blocked_by: Vec<TicketKey>,   // domain is a VECTOR; edge table is ledger-only
    // cached tracker mirror (observation + when):
    title: Option<String>, assignee: Option<String>,   // assignee → ours vs peer on the board
    status: Option<TicketStatus>, synced_at: Option<OffsetDateTime>,
}
// port: TicketRepo { upsert, get, by_epic, blockers_of, dependents_of, frontier }
```
```sql
-- crates/ledger
CREATE TABLE tickets (
    key TEXT PRIMARY KEY,
    epic TEXT REFERENCES tickets(key),
    held INTEGER NOT NULL DEFAULT 0,            -- SQLite has no bool → 0/1
    created_at TEXT NOT NULL,
    title TEXT,
    assignee TEXT,                              -- cached; ours vs peer on the board
    status TEXT CHECK (status IN ('todo','in_progress','in_qa','done','cancelled')),
    synced_at TEXT
);
CREATE TABLE ticket_blocks (                    -- persistence of blocked_by; ledger-only
    ticket TEXT NOT NULL REFERENCES tickets(key),
    blocked_by TEXT NOT NULL REFERENCES tickets(key),
    PRIMARY KEY (ticket, blocked_by), CHECK (ticket <> blocked_by)
);
```
- **`classification` removed** — retires `mady-implement`'s classify+persona+shape
  routing for one `/goal <ticket>` skill (task #6; aligns with TODO F1).
- **`review_gate` moved off the ticket** → the `order` (as `auto_review`). A domain
  entity carries neither *who* implements nor *how* it's reviewed — that's dispatch policy.
- **`blocked_by` is a `Vec<TicketKey>` in the domain**; the `ticket_blocks` join
  table is the *ledger* encoding (SQL has no list column). Table > JSON blob:
  bidirectional queries, `frontier()` = one JOIN (replaces `epic.py frontier` +
  `candidate.sh`), FK integrity (no dangling edge — the prose↔edge drift class), and
  single-row INSERT/DELETE (no read-modify-write race).
- **facts** = key/epic/held/created_at + edges · **cache** =
  title/assignee/status/synced_at · **derived** = is_blocked, frontier, ours-vs-peer.

### `ship` + `ship_snapshot` — the MR the crew builds & sails into port — DECIDED 2026-09-29

The worker *builds* the ship; the sailor *sails it into merge* ("ship it"). **One
`ship` shape serves the whole armada** (ours + peers), differentiated by `pilot`
(author) + nullable `ticket` — no peer table. Split by **volatility**: stable facts
in `Ship`, the churny forge mirror in a 1:1 `ShipSnapshot` (re-synced, stale past
`synced_at`). Coupling lives only in a read-layer `ShipView`. This is where the
arm-state lie lives (#49/#51).

```rust
// crates/core
pub enum ShipState      { Draft, Open, Merged, Closed }
pub enum PipelineStatus { Running, Success, Failed, Canceled, Manual }
pub struct Approvals    { current: i64, required: i64 }

pub struct Ship {                     // stable facts — written at birth + end
    id: ShipId,
    ticket: Option<TicketKey>,        // None for peer ships
    iid: i64, url: String, target: String,
    pilot: String,                    // author — ours vs peer differentiator
    created_at, ended_at,
}
pub struct ShipSnapshot {             // churny forge mirror, 1:1; one-way ref → ship
    ship: ShipId,
    title: String, state: ShipState, head_sha: String,
    has_conflicts: bool, open_threads: i64,
    approvals: Approvals, armed: bool, on_merge_train: bool,
    pipeline: Option<PipelineStatus>, synced_at,
}
pub struct ShipView { ship: Ship, snapshot: Option<ShipSnapshot> }  // read-layer ONLY
// ports: ShipRepo { upsert, get, for_ticket, open() } · SnapshotRepo { sync, of(ShipId) }
```
```sql
-- crates/ledger
CREATE TABLE ships (
    id INTEGER PRIMARY KEY,
    ticket TEXT REFERENCES tickets(key),         -- nullable (peer ships)
    iid INTEGER NOT NULL UNIQUE,
    url TEXT NOT NULL, target TEXT NOT NULL,
    pilot TEXT NOT NULL,                           -- author (ours vs peer)
    created_at TEXT NOT NULL, ended_at TEXT
);
CREATE TABLE ship_snapshots (
    ship_id INTEGER PRIMARY KEY REFERENCES ships(id),   -- PK=FK ⇒ enforces 1:1
    title TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('draft','open','merged','closed')),
    head_sha TEXT NOT NULL,
    has_conflicts INTEGER NOT NULL, open_threads INTEGER NOT NULL,
    approvals_current INTEGER NOT NULL, approvals_required INTEGER NOT NULL,
    armed INTEGER NOT NULL, on_merge_train INTEGER NOT NULL,
    pipeline TEXT CHECK (pipeline IN ('running','success','failed','canceled','manual')),
    synced_at TEXT NOT NULL
);
```
- **armada = the `ships` table** (all open MRs); **fleet = ours** (derived filter on
  `pilot`/`ticket`). Replaces `mrs.json`.
- **facts** (`ships`): ticket/iid/url/target/pilot/created_at/ended_at.
- **cache** (`ship_snapshots`, `synced_at`): the rest — re-synced; poll-churn rewrites
  only this table (stable facts untouched).
- **derived (never stored):** `mergeable_now`, `armed_now`,
  `blocking_discussions_resolved` (`open_threads==0`) — from a **fresh** sync at
  decision time (armed via GraphQL, #51).
- **not stored** (re-polled on action): `failed_jobs`, `source_branch`, `updated_at`,
  `pipeline sha`, `user_notes_count`, `threads_total`, raw `detailed_merge_status`.
- FK naming: `ship_id` (SQL) / `ship: ShipId` (Rust); `ticket` stays bare (natural key).

### `escalation` — a wall needing operator/SME judgment — DECIDED 2026-09-29

No `kind` (categorization is a **live drain judgment**, not a stored guess — same
lesson as `state`; drain paths can't be pre-enumerated). No `review` (inspector
removed → principal-reviewer is a *live* stamp). Resolution folded in via
`ended_at` (absorbs the old `resolutions/` sidecar).

```rust
// crates/core
pub struct EscalationId(pub i64);
pub struct Escalation {
    id: EscalationId,
    ticket: TicketKey,
    raised_by: CrewKind,       // worker | sailor | hand
    summary: String,           // the claim
    evidence: Vec<String>,     // cited backing (file:line, logs, API, doc §) — "cite, never assert"
    ship_iid: Option<i64>,
    phase: Option<String>,
    created_at, ended_at,      // raised / drained (None = open)
    resolution: Option<String>,
}
// port: EscalationRepo { raise, resolve, get, open(), for_ticket }
```
```sql
-- crates/ledger
CREATE TABLE escalations (
    id INTEGER PRIMARY KEY,
    ticket TEXT NOT NULL REFERENCES tickets(key),
    raised_by TEXT NOT NULL CHECK (raised_by IN ('worker','sailor','hand')),
    summary TEXT NOT NULL,
    evidence_json TEXT NOT NULL DEFAULT '[]',   -- opaque payload; JSON, not a child table
    ship_iid INTEGER, phase TEXT,
    created_at TEXT NOT NULL, ended_at TEXT,     -- ended_at NULL = open
    resolution TEXT
);
```
- **`evidence`** = cog-supplied citations at raise-time (via the escalate verb) —
  the "cite, never assert" rule. Opaque payload → JSON (never queried into).
- classification + validation are **live at drain** (foreman + principal-reviewer);
  the outcome lands in `resolution`. `is_open` derived (`ended_at IS NULL`).

### `event` — the CDC change-log (the nervous system) — DECIDED 2026-09-29

**New to MADY** (imported from apiary). Every mutation to any state table appends
one event **in the same transaction** → the log can never lie about state. Purpose:
(1) cheap **live TUI updates** via a follow-cursor, (2) an **audit/debug trail**
(ordered history per entity), (3) **no-op-tick detection**. **Not** event-sourcing —
the state tables stay primary; events are a log *beside* them. Model: state tables =
the body (what *is*); events = the nervous system (what *changed*, in order).

```rust
// crates/core
pub struct Seq(pub i64);          // monotonic follow-cursor (also the PK)
pub enum EntityKind { Session, Ticket, Ship, Escalation, Ticker }
pub enum EventKind {              // a FACT (deterministic mutation type), grows as needed
    SessionStarted, SessionEnded, TicketCreated, TicketSynced,
    TicketBlocked, TicketUnblocked, ShipOpened, ShipSynced, ShipMerged,
    ShipClosed, EscalationRaised, EscalationResolved, TickRan,
}
pub struct Event {
    seq: Seq, created_at,         // order+cursor / wall-clock
    kind: EventKind,
    entity_kind: EntityKind, entity_ref: String,   // polymorphic (stringified id/key)
    payload: serde_json::Value,  // the delta — opaque
}
// port: EventLog { append(&Event) /* SAME txn as the mutation */,
//                  since(Seq), for_entity(EntityKind, &str) }
```
```sql
-- crates/ledger
CREATE TABLE events (
    seq          INTEGER PRIMARY KEY AUTOINCREMENT,   -- monotonic, never reused (cursor-safe)
    created_at   TEXT NOT NULL,
    kind         TEXT NOT NULL,                        -- 'ship.merged', 'session.ended', …
    entity_kind  TEXT NOT NULL
                 CHECK (entity_kind IN ('session','ticket','ship','escalation','ticker')),
    entity_ref   TEXT NOT NULL,                        -- stringified id/key (polymorphic; no FK)
    payload_json TEXT NOT NULL DEFAULT '{}'
);
CREATE INDEX events_by_entity ON events(entity_kind, entity_ref);
```
- **`seq`** = the follow cursor (`GET /events?since=<seq>`); `AUTOINCREMENT` is the
  one legit use here — never reuse a seq, so a follower can't be fooled.
- **`kind` here IS a fact** (deterministic mutation), unlike the escalation `kind` we
  dropped (a judgment).
- **polymorphic ref** (`entity_kind`+`entity_ref`, no FK) — the standard event-log
  trade-off, since SQL can't FK to one-of-many tables.
- **retention:** capped by a periodic `DELETE` tick (not logrotate).
- **`tick` is NOT a table — it's a `TickRan` event.** Every scheduler run logs one
  (entity_kind `ticker`, entity_ref = ticker name), payload `{outcome, version_before,
  version_after}`. No-op = `before==after`; **spine-frozen = no recent `TickRan`**
  (catches #56, converge silently halting). Productive ticks also emit their own
  mutation events.

### `order` — the dispatch queue (operational INTENT) — DECIDED 2026-09-30

The "dispatch this" packet, in a **queryable queue**. Replaces today's `dispatch/*.json`
packets + the `queue/*.md` launched-tracking — no markdown parsing; "launched" is a
JOIN. **Operational intent** (the daemon *actuates* it) → a ledger table, unlike
*static authored* intent (prompts, grooming scope) which stays files.

```rust
// crates/core
pub struct Order {
    ticket: TicketKey,        // PK + FK → tickets (one active order per ticket)
    auto_impl: bool,          // true = worker (auto) · false = hand (operator builds)
    auto_review: bool,        // true = un-drafted, finisher merges · false = held in draft (human review)
    fork_from: String,        // stacking base (branch/sha to cut the worktree from)
    ac_summary: String,       // frozen brief handed to the crew at commission
    park_at: Option<String>,
    created_at,               // commissioned (today's dispatched_at)
}
// port: OrderRepo { commission, get, for_ticket, queue() }
```
```sql
-- crates/ledger
CREATE TABLE orders (
    ticket      TEXT PRIMARY KEY REFERENCES tickets(key),   -- one active order per ticket
    auto_impl   INTEGER NOT NULL DEFAULT 1,                  -- 0/1 (default worker)
    auto_review INTEGER NOT NULL DEFAULT 1,                  -- 0/1 (default auto-merge)
    fork_from   TEXT NOT NULL,
    ac_summary  TEXT NOT NULL,
    park_at     TEXT,
    created_at  TEXT NOT NULL
);
```
- **launched = derived** (a `session` exists for the ticket) — never stored.
- **frontier (eligible) = derived from `tickets`** (Ready + unblocked + no order) — what
  *could* be commissioned; replaces `epic.py frontier` + `candidate.sh`.
- **dropped from today's packet:** `classification` (gone), `review_gate`→`auto_review`,
  `impl`→`auto_impl`, `block_on` (derive from `ticket_blocks`), `rebase_loop` (config).
- **reconcile (flagged):** `worktree_path` — cut at commission, so likely on `order`
  with `session` deriving it; one home, not two.
- both booleans **default `true`** (normal auto path); `false` for hand-built / human-reviewed.

### `liveness` — derived from a background poll (NO heartbeat table) — DECIDED 2026-09-30

There is **no `heartbeat` entity/table.** Instead:
- **`SessionHost` gains `last_activity(session) -> Option<ts>`** — tmux impl reads
  `#{window_activity}` (last pane output); a CPU sample can be folded in *inside the
  adapter* later without touching `core`. (For Claude-Code cogs, `#{window_activity}`
  is a good-enough proxy — a conversation *is* output, and the TUI redraws during tool
  calls — so CPU stays in the back pocket.)
- The daemon runs a **background poll (~5s)** reading `last_activity` per session into an
  **in-memory** map. This is the *one* place the tmux fan-out runs — **never per query**
  (apiary's lag-bug lesson). No DB writes; a restart just re-polls.
- **`Liveness { Active, Quiet, Parked, Dead }` is derived at read time** in `core` from
  `age of max(in-mem activity, latest event)` vs thresholds (active/quiet); `Dead` =
  `!alive` + non-terminal state. Never stored.
- The **`context` endpoint returns liveness fresh** (cheap read of the poll cache +
  derive) — same info `context.sh` gives today, but accurate-by-construction.
- A liveness **class change emits an `event`** (active→quiet…) so the TUI follow-stream
  updates without re-polling.
- Promote to a durable table only if we ever want activity *history* / restart-survival —
  we don't.

### Ports — the driven adapters (swap the stack, don't touch `core`) — DECIDED 2026-09-30

Three ports in `core`, each named for its **role, never the vendor** (R2/R3). The
vendor lives only in the impl; swapping stacks = one new impl, `core`/entities
unchanged.

| Port | Role (category) | Impl today | Transport | Swap example |
|---|---|---|---|---|
| **`Forge`** | code-hosting platform: repo + ships(MRs) + pipelines + reviews | `GlabForge` | **shell out to `glab`** (holds auth + platform lore) | `GithubForge` (`gh`) |
| **`Tracker`** | issue/work tracker: tickets + status + sprints | `JiraTracker` | **native REST + PAT** (headless; fixes #47/#48) | `LinearTracker` |
| **`SessionHost`** | substrate that launches + tracks live sessions | `TmuxSessionHost` | **shell out to `tmux`** | `DockerSessionHost` / `SshSessionHost` |

- "**Forge**" = the established vendor-neutral term for a git platform (SourceForge
  lineage) — covers GitLab/GitHub/Gitea in one word.
- **Transport principle** (from apiary): **shell out when a mature CLI already holds
  the auth + encodes the platform's traps** (`glab`, `git`, `tmux`); **go native only
  where the CLI/MCP path is worse for a headless daemon** (Jira → REST+PAT, since a
  daemon can't do the browser OAuth the Atlassian MCP needs).
- Traits live in `core`; impls are the drivers (in `fleetd` behind the ports).

## 4. Migration approach — the strangler-fig

Copy apiary's discipline: convert *without a big-bang cutover*.

1. **Legacy mirror.** The daemon keeps writing the old `~/.mady/state/*.json`
   *after* committing to the DB, so `context.sh`, the dashboard, and converge's
   readers keep working untouched during the transition.
2. **Dual-mode reader + differential tests.** A client reads DB *or* files and
   the two are **byte-identical**, pinned by tests. Nothing cuts over until proven
   equivalent.
3. **Running work keeps old behavior until relaunched.** No hot-swap under a live
   cog.
4. **Design constraints honored from day one** (so a later remote box is drop-in):
   logical keys not `/Users/...` paths in the DB; UDS-only; new code Linux-clean
   (no bash-3.2/BSD-`date`/`stat -f` assumptions); daemon is a plain foreground
   process (launchd now, systemd later).

---

## 5. Phased plan

- **Phase 0 — read-only daemon + first client (the safe spike).** Daemon v0:
  DB + UDS API + read-only session registry. A one-shot importer ingests
  `~/.mady/state/` into the DB. Point **`crew/`** at it (`--bk` with file
  fallback). *Nothing writes through the daemon.* **Exit:** `crew` renders
  byte-identical from DB vs files. Touches no cog, no converge path.
- **Phase 1 — daemon writes + CLI shims.** Console verbs become the write path;
  status-drop scripts become thin shims. Daemon mirrors the JSON. converge still
  paces.
- **Phase 2 — scheduler absorbs cron.** Move `loop/*-tick.sh` into the scheduler
  one ticker at a time — **`jira-fetch` and `mrs-fetch` first** (pure caches,
  lowest risk). Crontab collapses toward one unit.
- **Phase 3 — reconcile + ports.** converge's dispatch/spawn/reap moves into the
  daemon behind forge/tracker adapters.
- **Phase 4 — retire fs-state + old scripts.** Delete, don't port.

---

## 6. Open questions

- [x] ~~Name the daemon + state~~ → **`fleetd`** + **`ledger`** (2026-09-29).
- [x] ~~Language~~ → **Rust** (2026-09-29).
- [x] ~~Placement~~ → **virtual Cargo workspace, all crates under `crates/`**
      (2026-09-29, §2.1).
- [x] ~~Name the console CLI~~ → **`fleetc`** (client to `fleetd`) (2026-09-29).
- [x] ~~Shared-crate split~~ → **`core`** (domain+ports) / **`ledger`** (sqlite
      adapter) / **`wire`** (socket types + client) (2026-09-29, §2.1).
- [ ] Name the **UDS API channel** + the **event stream** (§2 table rows still open).
- [x] ~~**Standing session shape**~~ → `sessions.ticket` **nullable**, ticket-binding enforced
      per-kind via a DB CHECK (worker/sailor require a ticket; hand/captain/singletons don't); a
      nullable `convoy` column (a `ConvoyKey`, adapter-neutral) keys the captain. (2026-10-16, §8.)
- [x] ~~**Metaphor coherence**~~ → **foreman → `captain`** (overseer re-themed maritime) + **the
      work grouping = `convoy`** (not Jira's `epic`); `the line`→`voyage` and
      `millwright`→`shipwright` noted as coherent follow-ons, not yet committed. (2026-10-16, §8.)
- [ ] **Multi-adapter origin** — once a Jira `Tracker` and a filesystem `Tracker` coexist, a ticket
      needs an **origin** tag so a transition routes to the right driver. Deferred (single-adapter
      today). (noted 2026-10-16.)
- [ ] Do our factory lifecycle semantics (foreman/finisher/inspector, escalation
      sidecars, review-gate=human) map cleanly onto apiary-style tables, or do we
      need our own schema shapes?
- [ ] How much of converge's reconcile is *rules we keep* vs *mechanism we move*?

---

## 7. Cut-over checklist

The swap is a **strangler gated by equivalence** (§4): fleet runs *beside* the bash
kit, proves byte-identical, then takes one surface at a time. Each box is a gate —
don't flip the next until the prior holds. "Delete, don't port" (§5 Phase 4).

**A. Read parity (no writes yet).**
- [ ] `fleetd sync` mirrors `board` + `armada` into the ledger on cadence.
- [ ] `crew`/`dashboard` read from `fleetd` (`--bk`) and render **byte-identical** vs
      `~/.mady/state/` for a full sprint day. *(Phase 0 exit.)*

**B. Status channel (per cog).** Repoint each cog's `snapshot_write` → `fleetc status`,
one cog at a time; while a cog is unmoved its JSON drop stays the source.
- [ ] worker → `fleetc status --cog worker`
- [ ] sailor (ex-finisher) → `fleetc status --cog sailor`
- [ ] hand (ex-human) → `fleetc status --cog hand`
- [ ] standing sessions (auditor/foreman) — needs **nullable-ticket sessions** (§6).
- [ ] last cog moved → delete `bin/lib/snapshot.sh` + `schemas/*-status.schema.json`.

**C. Reconcile equivalence (dry-run beside converge).**
- [ ] reconcile runs `dry_run=true` on cadence, logging the planned `Action[]` as a
      `TickRan` event *(done — `reconcile::tick_dry_run`)*.
- [ ] diff fleet's plan vs converge's actual dispatch/handoff/reap/heal per cycle.
- [ ] **Gate:** zero divergence across a representative window — a block→unblock, an
      escalation, a crash→heal, a concluded→handoff, a merge→reap.

**D. The flip (writes on).** Build the executor behind the `dry_run` flag (apply
Dispatch/Handoff/Reap/Heal through the ports).
- [ ] executor applies each action and is **idempotent at restart** — re-derives,
      never double-acts; the `one_live_crew_per_ticket` lock backstops double-dispatch.
- [ ] flip `dry_run=false` and **disable converge's reconcile** (fleet owns the Jira
      transitions); cron stays only for surfaces not yet moved.

**E. Scheduler absorbs cron (§5 Phase 2).** Move `loop/*-tick.sh` into the scheduler
one ticker at a time — `jira-fetch` + `mrs-fetch` first (pure caches, lowest risk).
- [ ] each ticker moved + its crontab line removed; the crontab collapses to one unit.

**F. Retire the old kit (delete, don't port).**
- [ ] inspector agent + `inspector-status.schema.json` (#4 — already absent from fleet).
- [ ] **`human` agent deleted** — *not* folded (hand stays lean); drop
      `human-status.schema.json` + the `impl=human` dispatch path (#5). First **inject**
      the in-flight human tickets as hand sessions (ticket + `worktree_path` only, no
      state) so reconcile heals them → relaunched as hands; operator re-briefs each.
- [ ] `mady-implement` classify+persona → one `/goal` skill (#6).
- [ ] `converge.sh`, `bin/lib/*`, `bin/adapters/*`, the fs-state importer, and the
      residual `bin/*.sh` commands absorbed by `fleetc`.
- [ ] `~/.mady/state/` JSON retired once nothing reads it.

**G. Bring-up + rollback.**
- [ ] a launchd unit runs `fleetd` (plain foreground; systemd later); `install.sh`
      stows `fleet.toml` to `~/.mady/`.
- [ ] rollback path = `--files` mode (the `fs_store` fallback), kept until a clean
      stability window closes it out.

---

## 8. Decisions log

_(date — decision — why. Append-only.)_

- 2026-09-29 — Build our own, do **not** re-use apiary; take inspiration only,
  keep the factory metaphor. — operator.
- 2026-09-29 — Daemon = **`fleetd`** (owns/paces the fleet of cogs; `+d` per Unix
  convention; theme-neutral "fleet of agents", not a role → won't be mistaken for a
  cog). Note: CoreOS shipped a defunct `fleetd` (~dead 2018), no live clash. —
  operator.
- 2026-09-29 — State = **`ledger`** (`ledger.db`), replacing the `~/.mady/state/`
  JSON. — operator.
- 2026-09-29 — Language **Rust**; a **virtual Cargo workspace** with all crates
  under `crates/`, no privileged root package. — operator.
- 2026-09-29 — Architecture: **domain-first / Hexagonal**. Rust domain types in
  `core` are the source of truth for *shape*; the SQLite `ledger` is a driven
  adapter behind a repository port; DB constraints (NOT NULL/CHECK/FK) carry
  *integrity*; the adapter is the seam that keeps the two agreeing. Typed Rust
  (+serde) replaces the JSON `schemas/`; the DDL adds the integrity layer. —
  operator.
- 2026-09-29 — Crate split: libs **`core`** (domain+ports) / **`ledger`** (sqlite
  adapter) / **`wire`** (socket types+client); apps **`fleetd`** / **`fleetc`** /
  **`crew`** / **`dashboard`**. Client CLI = **`fleetc`** (fleetd↔fleetc =
  server/client). — operator.
- 2026-09-29 — `crew`/`dashboard` get re-adapted in a later phase: a new
  fleet-client adapter behind their *existing* ports (over `wire`), keeping
  `fs_store` as the legacy fallback → the dual-mode strangler falls out of the
  Clean-Arch already in place. — operator.
- 2026-09-29 — Workspace layout stays **flat** (`crates/*`); revisit grouping only
  past ~a dozen crates. — operator.
- 2026-09-29 — Agents are the **crew** (drop "cogs"). **Inspector removed** as agent
  + escalation mechanism (principal-reviewer is the validation stamp; task #4).
  **`Human`→`Hand`** and the `human` agent collapses into `hand` as a two-mode agent
  (task #5). Crew roles: **Worker / Sailor / Hand**. — operator.
- 2026-09-29 — First entity encoded: **`sessions`** (§3.1) — lean fact struct +
  table, DB-enforced spawn lock. — operator.
- 2026-09-29 — Standing rule: **no vendor leak in `core`** — entities are
  domain-named; the vendor lives only in its adapter (`TicketStatus` not
  `JiraStatus`; `vessel` not `merge_request`). — operator.
- 2026-09-29 — `ticket` entity encoded (§3.1). `classification` **dropped** (retire
  `mady-implement` classify+persona → one `/goal` skill, task #6). `blocked_by` =
  domain vector, `ticket_blocks` join table in the ledger only. — operator.
- 2026-09-29 — MR entity = **`ship`** (worker builds it, sailor "ships it" into
  port/merge; vendor-neutral). Arm-state is cache + re-polled, never stored-trusted
  (#49/#51). — operator.
- 2026-09-29 — Standing rule: **canonical timestamps** `created_at` / `ended_at` /
  `synced_at` across all entities; ids = `id` (synthetic) vs `key` (natural). Applied
  back to `sessions` (started_at→created_at) + `tickets` (first_seen_at→created_at). —
  operator.
- 2026-09-29 — `escalation` entity encoded (§3.1). `kind` **dropped** (categorization
  is a live drain judgment, not a storable pre-known taxonomy); `review`/inspector
  refuter **dropped**; resolution folded in via `ended_at` (absorbs `resolutions/`). —
  operator.
- 2026-09-29 — `event` entity encoded (§3.1) — NEW (from apiary). CDC change-log
  appended in the same txn as each mutation; `seq` follow-cursor (`AUTOINCREMENT`),
  `EventKind` core enum, polymorphic `entity_kind`+`entity_ref` (no FK). Purpose:
  live TUI updates + audit trail + no-op-tick detection. **Not** event-sourcing —
  state tables stay primary. — operator.
- 2026-09-30 — **No `tick` table** — a tick is a **`TickRan` event** (payload carries the
  `version_before/after` span → no-op detection; absence → spine-frozen, #56). FACTS
  bucket complete. — operator.
- 2026-09-30 — **No `heartbeat` table.** Liveness (§3.1) = derived at read time from a
  daemon **background poll (~5s)** of `SessionHost::last_activity` (tmux `#{window_activity}`;
  CPU later, adapter-internal) held **in memory**, plus the latest event. Poll = the only
  place the tmux fan-out runs (never per query). `context` returns it fresh; class-change
  emits an event. — operator.
- 2026-09-30 — Board vs intent kept **separate** (never dispatch from the sprint mirror):
  `board` = the `tickets` sprint-mirror (view/reality); the **dispatch queue = `order`**
  (operational intent). `ticket` stays a pure domain entity — `review_gate` **removed**
  from it (→ `order.auto_review`), no `implementer` added. `order` carries `auto_impl` +
  `auto_review` (booleans, default true), `fork_from`, `ac_summary`, `park_at`. Launched +
  frontier are **derived**. Intent splits: static-authored → files; operational (the
  queue) → ledger. — operator.
- 2026-10-01 — Tracker transport **refined**: reads + transitions **shell out to
  `jira-cli`** (headless token auth, handles the corp TLS/CA that trips raw HTTP —
  TODO #48; consistent with glab/git/tmux). **Native REST+PAT reserved for the buggy
  writes** (component / story-point sets, #47/#48) at ticket-creation time. — operator.
- 2026-09-30 — Ports (§3.1): **`Forge`** (code platform) / **`Tracker`** (issue tracker)
  / **`SessionHost`** (session substrate) — role-named, vendor-neutral, so the stack is
  swappable. Transport: shell out to `glab`/`git`/`tmux` (CLI holds auth+lore); native
  REST+PAT for Jira. — operator.
- 2026-09-30 — Scope: **`fleetd` absorbs most/all of `bin/`** (loop ticks, converge +
  foreman features, lib, adapters → Rust in fleetd behind ports; operator commands →
  `fleetc`). `bin/` largely dissolves. Per-driver choice: shell out to `glab`/`jira`/
  `tmux` vs native API client. — operator.
- 2026-09-29 — `ship` redesigned (§3.1): split into **`Ship`** (stable facts) +
  **`ShipSnapshot`** (1:1 churny forge mirror) by volatility; coupling only in a
  read-layer **`ShipView`**. One shape for the whole armada — `ticket` nullable +
  **`pilot`** (author) differentiates ours/peer; no peer table. FK column = `ship_id`
  (SQL) / `ship: ShipId` (Rust); `ticket` stays bare. — operator.
- 2026-09-29 — Maritime work vocabulary: **`ship`** = one of our MRs · **`fleet`** =
  our group of ships sailing to the **harbor** (merge) · **`armada`** = the larger
  superset of ALL open ships (ours + peers) the **auditor** patrols · **`board`** =
  all sprint tickets (Jira view). `board` + `armada` are the two external-reality
  structures replacing `jira.json`/`mrs.json`. — operator.
- 2026-10-05 — **`human` fully purged** (task #5), *not* made a hand "mode": delete the
  `human` agent + `human-status.schema.json` + `impl=human` path at cutover (it stays
  live under converge for the in-flight tickets until then). Operator-driven per-ticket
  work = a **lean `hand` launched into a foreman-prepared worktree** — the dispatch
  packet carries the behavior, so `hand.md` stays minimal + flexible for one-offs incl.
  the human-like case (no two-mode persona). In-flight human tickets migrate via a
  one-shot **injection** (minimal: `ticket` + `worktree_path` → a live `hand` session,
  no state carried); at cutover reconcile heals them (tmux-Dead, unpaused) → relaunched
  as hands, operator re-briefs each. Ad-hoc `hand` is **untracked** (no session row).
  `MADY-5250` excluded (still running under the old finisher). — operator.
- 2026-10-16 — **`sessions.ticket` nullable; ticket-binding enforced per kind** (resolves the §6
  standing-session-shape question; built this session). The blanket `NOT NULL` was over-strict:
  worker/sailor MUST have a ticket, hand MAY (dispatched or one-off), captain + standing singletons
  (auditor/shipwright) have none. Enforced by a DB CHECK `(ticket IS NOT NULL OR kind NOT IN
  ('worker','sailor'))`, keeping the DB the integrity layer. A nullable **`convoy`** column keys the
  captain (see the convoy entry below) with a second CHECK `(kind <> 'captain' OR convoy IS NOT
  NULL)`: `one_live_crew_per_ticket` still locks ticket-cogs (NULLs are distinct in a unique index,
  so ticketless cogs don't collide there), and a new partial unique index
  `one_live_captain_per_convoy ON sessions(convoy) WHERE kind='captain' AND ended_at IS NULL` gives
  one-live-captain-per-convoy. `CrewKind` gains `Captain`. — operator.
- 2026-10-16 — **Grouping concept = `convoy` (not `epic`); new `ConvoyKey` newtype** (built this
  session). "Epic" is Jira's own product noun, so it can't be the *domain* name for an
  adapter-neutral concept — a Jira epic and a filesystem folder are two **adapter realizations** of
  one grouping (the no-vendor-leak-in-`core` rule, 2026-09-29). The domain model: a **convoy** (the
  goal) → ordered, dependency-linked **tickets** (atomic units, each manned by a sailor on a ship);
  a captain commands the convoy. Realized per-adapter behind the `Tracker` port — Jira:
  epic→convoy, issue→ticket, issue-link→blocked_by; filesystem: folder→convoy, file→ticket,
  frontmatter→blocked_by — and they can coexist (the ledger stores domain facts; `Tracker` is a
  port). `Ticket.epic: Option<TicketKey>` → `Ticket.convoy: Option<ConvoyKey>` — its own key, since
  a convoy is NOT a ticket (each adapter supplies the identifier), so the old `tickets.epic` FK to
  `tickets(key)` is dropped. — operator.
- 2026-10-17 — **Captain staffing** (the `reconcile_foremen` port; built this session). Convoy
  **existence is an operator fact, completion is the captain's judgment**: `fleetc convoy open`
  creates an active convoy (new `convoys` table + `Convoy` entity + `ConvoyRepo` open/close/active,
  CDC `ConvoyOpened`/`ConvoyClosed`); reconcile's new `StaffCaptain`/`ReapCaptain` run one captain
  per active convoy — launched ticketless at `config.hub` (the bash `$MADY_REPOSITORY`), reaped
  when the convoy is no longer active. The captain closes its own convoy (`fleetc convoy close`)
  once all tickets are delivered → self-terminating (next tick reaps it). `close` is unconditional
  at the mechanics level — the "all delivered" check is the captain agent's judgment. Before *live*
  staffing: the `foreman.md → captain.md` agent rename (`launch_cmd` → `claude --agent captain`);
  the mechanics build + test under `dry_run`. — operator.
- 2026-10-17 — **Jira claim follow-ons** (the `claim.sh` port; built this session). `spawn` was
  transition-only; now it runs the full claim: **gate → transition → assign/label/sprint**. The
  `Tracker` port gains `assigned_to_peer` (step 0 — a `currentUser()` JQL; **never stomp a peer**,
  decided with the operator), `assign_to_me`, `add_label`, `add_to_active_sprint`. The gate is
  fatal-closed (can't verify ownership → don't claim); the transition stays the lock; the three
  follow-ons are best-effort (a partial claim is logged, not fatal — the ticket is locked and the
  cog must still launch). — operator.
- 2026-10-17 — **Anchor** (ex-"park"; the `park.sh`/`cog_is_parked` port; built this session). The
  anchor is a **fact on the session** (`sessions.anchored_at`) — single source of truth + audit
  stamp. The cog never touches SQLite: reconcile/scheduler is the bridge — `fleetc anchor <MADY-N>`
  stamps the ticket's live cog(s) and pushes a graceful stop signal via tmux `SessionHost::send`
  (the cog checkpoints + reports `anchored`). `heal` never respawns an
  `anchored_at IS NOT NULL` session (the authoritative hold, even before the cog self-reports).
  **Anchoring is graceful-only** — tearing a cog down hard is a *separate, deliberate verb*,
  `fleetc sink <MADY-N>` → `Command::Sink` → end the session + kill the tmux (2026-10-18; a kill is
  too dangerous to hide behind an anchor flag — operator call).
- 2026-10-18 — **Parent roll-up** (converge `rollup_parent`; built this session). On `Reap`, after a
  sub-task is transitioned Done, close its parent *story* iff ALL the story's sub-tasks are terminal
  (the `!subtasks.is_empty()` guard skips Epics — no native sub-tasks). Ported **live**, not cached:
  `Tracker` gains `parent_of` + `subtasks`, and completeness is checked against Jira at reap time —
  caching off the sprint-scoped ledger would risk a premature close when a sibling sub-task sits in
  the backlog. Best-effort (a tracker hiccup or a redundant two-siblings-at-once close never fails
  the reap). This is the story→sub-task edge — **distinct from the `convoy`/epic**, which the captain
  closes with judgment (`fleetc convoy close`), not an auto-roll-up. — operator.
  `fleetc weigh <MADY-N>` (weigh anchor = get under way) clears it + nudges resume. CDC
  `SessionAnchored`/`SessionWeighed`. Naming, decided with the operator: `Liveness::Parked → Idle`
  (the derived "idle past quiet_secs" tier — *observability only, nothing branches on it*, so the
  name was free to take), and the cog-report `WorkerState/HandState::Parked → Anchored`. The order's
  `park_at` (the *planned* pre-MR hold) stays separate + unenforced. — operator.
- 2026-10-16 — **Foreman → `captain`** in the fleet (factory→fleet maritime coherence; resolves the
  §6 metaphor question for the overseer). Same job — per-epic orchestration + resolution authority +
  operator console — maritime name: one captain per epic line (voyage). `the line`→`voyage` and
  `millwright`→`shipwright` are coherent follow-ons, noted but not committed. The bash `foreman` keeps
  its name until cutover. — operator.
