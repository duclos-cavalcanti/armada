-- fleet ledger schema (PLAN §3.1). The single source of truth for STATE shape;
-- integrity lives here (NOT NULL / CHECK / FK), the domain type is the shape.
-- Facts are stored; status/liveness/mergeable are derived at read time.

-- The work order + the board (all sprint tickets, upsert-filled by the tracker).
CREATE TABLE IF NOT EXISTS tickets (
    key         TEXT PRIMARY KEY,                     -- '<PREFIX>-<n>', e.g. 'PROJ-1234'
    convoy      TEXT,                                 -- nullable; the TRACKER's grouping (its epic), ticket-key shaped (NOT a ticket) — `convoy_members` can override it
    held        INTEGER NOT NULL DEFAULT 0,           -- held off the dispatch frontier (0/1)
    created_at  TEXT NOT NULL,
    -- cached tracker mirror (re-synced; the tracker is the source of truth):
    title       TEXT,
    assignee    TEXT,                                 -- ours vs allied on the board
    status      TEXT CHECK (status IN ('todo','blocked','in_progress','in_qa','done','cancelled')),
    synced_at   TEXT
);

-- is-blocked-by edges (the ledger encoding of Ticket.blocked_by).
CREATE TABLE IF NOT EXISTS ticket_blocks (
    ticket     TEXT NOT NULL REFERENCES tickets(key),
    blocked_by TEXT NOT NULL REFERENCES tickets(key),
    PRIMARY KEY (ticket, blocked_by),
    CHECK (ticket <> blocked_by)                      -- no self-block
);

-- Running crew members. ended_at NULL = live (the only lifecycle fact).
CREATE TABLE IF NOT EXISTS sessions (
    id                 INTEGER PRIMARY KEY,
    kind               TEXT NOT NULL CHECK (kind IN ('worker','sailor','hand','captain','lookout','admiral')),
    ticket             TEXT REFERENCES tickets(key),    -- nullable; worker/sailor/admiral require one (CHECK below)
    convoy             TEXT,                             -- nullable; a captain commands one (CHECK below)
    worktree_path      TEXT NOT NULL,
    tmux_session       TEXT NOT NULL,
    harness_session_id TEXT,
    created_at         TEXT NOT NULL,
    ended_at           TEXT,
    anchored_at        TEXT,                            -- operator anchor (stop & hold); NULL = under way
    ship               INTEGER REFERENCES ships(id),     -- the MR a lookout reviews
    -- a worker/sailor/admiral is bound to a ticket; a captain/hand/lookout may be ticketless.
    CHECK (ticket IS NOT NULL OR kind NOT IN ('worker','sailor','admiral')),
    -- a captain commands a convoy.
    CHECK (kind <> 'captain' OR convoy IS NOT NULL)
);
-- DB-enforced spawn lock: one LIVE ticket-cog per (ticket, kind). NULLs are distinct
-- in a unique index, so ticketless cogs (captains) never collide here — they get their
-- own lock below.
CREATE UNIQUE INDEX IF NOT EXISTS one_live_crew_per_ticket
    ON sessions(ticket, kind) WHERE ended_at IS NULL;
-- ...and one LIVE captain per convoy.
CREATE UNIQUE INDEX IF NOT EXISTS one_live_captain_per_convoy
    ON sessions(convoy) WHERE kind = 'captain' AND ended_at IS NULL;
-- ...and one LIVE lookout per allied ship it reviews.
CREATE UNIQUE INDEX IF NOT EXISTS one_live_lookout_per_ship
    ON sessions(ship) WHERE kind = 'lookout' AND ended_at IS NULL;

-- Convoys — the operator-curated grouping a captain commands. Existence is an operator
-- fact (open → active); `ended_at` is the captain's "all delivered" close, which reconcile
-- reads to reap the captain. Not FK'd to tickets: a convoy key is adapter-neutral.
CREATE TABLE IF NOT EXISTS convoys (
    key         TEXT PRIMARY KEY,
    title       TEXT,
    created_at  TEXT NOT NULL,
    ended_at    TEXT,                                 -- NULL = active
    anchored_at TEXT,                                 -- operator anchor (hold the captain); NULL = under way
    no          INTEGER,                              -- its short number (#N): in opening order, never reused
    goal        TEXT REFERENCES tickets(key)          -- the ticket whose Done proves its outcome; NULL = no critical path
);
CREATE UNIQUE INDEX IF NOT EXISTS convoy_no ON convoys(no);

-- A convoy's plan — the fabric tying its tickets together (outcome, sequencing, the invariants
-- across them, the rulings made), whatever the tracker. Set by `convoy open --plan`; no row = no
-- plan. The captain reads it with its slice.
CREATE TABLE IF NOT EXISTS convoy_plans (
    convoy     TEXT PRIMARY KEY REFERENCES convoys(key),
    body       TEXT NOT NULL CHECK (length(trim(body, char(32, 9, 10, 13))) > 0),  -- blank ≠ no plan
    updated_at TEXT NOT NULL
);

-- Convoy membership the fleet sets beyond the tracker's grouping. A ticket's convoy is the one it was
-- added to here, else its epic's (`tickets.convoy`) — and only one (the PK): a convoy can take on a
-- ticket from another epic it depends on, which can't be re-parented, and that ticket leaves its
-- epic's convoy for the fleet. Not FK'd to convoys: membership can precede `convoy open`.
CREATE TABLE IF NOT EXISTS convoy_members (
    ticket   TEXT PRIMARY KEY REFERENCES tickets(key),
    convoy   TEXT NOT NULL,
    added_at TEXT NOT NULL
);

-- Tickets the operator works by hand (`convoy add --hand`): the captain dispatches them to a hand,
-- not a worker.
CREATE TABLE IF NOT EXISTS hand_tickets (
    ticket    TEXT PRIMARY KEY REFERENCES tickets(key),
    marked_at TEXT NOT NULL
);

-- The lookouts' own control row: the hold over the whole review loop has no existing row to hang
-- on, so this one-row table IS its home. `anchored_at` = operator anchor on the review loop (NULL =
-- under way), which reconcile's lookout launch gate reads. One row, seeded at init.
CREATE TABLE IF NOT EXISTS lookout (
    id          INTEGER PRIMARY KEY CHECK (id = 1),   -- singleton: exactly one row
    anchored_at TEXT
);
INSERT OR IGNORE INTO lookout(id, anchored_at) VALUES (1, NULL);

-- A session's latest self-report (PK=FK = one latest row per session; replaced on
-- each report). `state` is the cog's string form — the per-cog enum validates it at
-- the fleetc write boundary, so no cog-gated CHECK here; readers widen it back using
-- the session's kind. Full history lives in the events log as 'status_reported'.
CREATE TABLE IF NOT EXISTS reports (
    session_id  INTEGER PRIMARY KEY REFERENCES sessions(id),
    state       TEXT NOT NULL,
    last_action TEXT,
    reported_at TEXT NOT NULL
);

-- Ships = all open MRs (ours + allies = the armada). Stable facts.
CREATE TABLE IF NOT EXISTS ships (
    id         INTEGER PRIMARY KEY,
    ticket     TEXT REFERENCES tickets(key),          -- nullable (allied ships)
    iid        INTEGER NOT NULL UNIQUE,
    url        TEXT NOT NULL,
    target     TEXT NOT NULL,
    source     TEXT NOT NULL DEFAULT '',               -- head branch (a stacked child's fork base)
    pilot      TEXT NOT NULL,                          -- author (ours vs allied)
    created_at TEXT NOT NULL,
    ended_at   TEXT,
    swept      TEXT,                                   -- fingerprint of the snapshot the last sailor was dispatched against
    reviewed   TEXT,                                   -- head_sha the last lookout reviewed (review cold-loop change-detector)
    rounds     INTEGER NOT NULL DEFAULT 0 CHECK (rounds >= 0) -- sailor sweeps launched on it (a review round each)
);

-- The churny forge mirror, 1:1 with a ship (PK=FK enforces one snapshot per ship).
CREATE TABLE IF NOT EXISTS ship_snapshots (
    ship_id            INTEGER PRIMARY KEY REFERENCES ships(id),
    title              TEXT    NOT NULL,
    state              TEXT    NOT NULL CHECK (state IN ('draft','open','merged','closed')),
    head_sha           TEXT    NOT NULL,
    has_conflicts      INTEGER NOT NULL,               -- 0/1
    open_threads       INTEGER NOT NULL,               -- unresolved review threads (merge-blocking)
    total_threads      INTEGER NOT NULL DEFAULT 0,     -- every review thread, resolved or not
    blocked_by_json    TEXT    NOT NULL DEFAULT '[]',  -- MR iids declared as blocking it, not merged; JSON
    approvals_current  INTEGER NOT NULL,
    approvals_required INTEGER NOT NULL,
    armed              INTEGER NOT NULL,               -- 0/1
    on_merge_train     INTEGER NOT NULL,               -- 0/1
    pipeline           TEXT    CHECK (pipeline IN ('running','success','failed','canceled','manual')),
    merge_status       TEXT,                           -- GitLab detailed_merge_status (open vocab; no CHECK)
    synced_at          TEXT    NOT NULL,
    threads_changed_at TEXT                            -- the sync that last saw its thread counts move (review settling)
);

-- The dispatch queue (operational intent). One active order per ticket.
CREATE TABLE IF NOT EXISTS orders (
    ticket      TEXT PRIMARY KEY REFERENCES tickets(key),
    auto_impl   INTEGER NOT NULL DEFAULT 1,            -- 0/1 (default worker)
    fork_from   TEXT NOT NULL,
    ac_summary  TEXT NOT NULL,
    park_at     TEXT,
    worktree_path TEXT,                            -- materialised by the producer; reconcile reads it at spawn, never creates it
    created_at  TEXT NOT NULL
);

-- Cases — an escalation start to finish: raised by a worker's or sailor's escalated report,
-- inspected by the lookout's verdict (`inspections.case_id`), ruled by the operator. Until it's
-- answered (a REFUTE, or a ruling), another escalation from its cog joins it, whatever its wording
-- (`reports` counts them), so it isn't judged twice; once answered, another escalation is a new
-- case. Any other report, or the session ending, settles it. A ruling with no escalation behind it has no
-- session and is settled at once. `convoy` = the ticket's when raised: a cog reads its own convoy's
-- cases before it escalates.
CREATE TABLE IF NOT EXISTS cases (
    id              INTEGER PRIMARY KEY,
    ticket          TEXT NOT NULL REFERENCES tickets(key),
    convoy          TEXT,
    session_id      INTEGER REFERENCES sessions(id),
    question        TEXT NOT NULL,
    raised_at       TEXT NOT NULL,
    reports         INTEGER NOT NULL DEFAULT 1,
    ruling_question TEXT,
    ruling          TEXT CHECK (ruling IS NULL OR length(trim(ruling)) > 0),
    ruled_at        TEXT,
    settled_at      TEXT,
    CHECK ((ruling IS NULL) = (ruled_at IS NULL) AND (ruling IS NULL) = (ruling_question IS NULL)),
    CHECK (session_id IS NOT NULL OR ruling IS NOT NULL)
);
CREATE INDEX IF NOT EXISTS cases_by_convoy ON cases(convoy, ticket);
CREATE UNIQUE INDEX IF NOT EXISTS one_open_case ON cases(session_id) WHERE settled_at IS NULL;

-- Inspections — verdicts on a cog's escalation (/fleet-inspect), each on its case: a fresh case has
-- none until it's inspected. `reported_at` = the report standing when it was judged.
-- CONFIRM: it stands, the operator's; REFUTE: its way out was sent to the cog.
CREATE TABLE IF NOT EXISTS inspections (
    id           INTEGER PRIMARY KEY,
    session_id   INTEGER NOT NULL REFERENCES sessions(id),
    case_id      INTEGER REFERENCES cases(id),
    reported_at  TEXT NOT NULL,
    verdict      TEXT NOT NULL CHECK (verdict IN ('confirm','refute')),
    body         TEXT NOT NULL CHECK (length(trim(body)) > 0),
    inspected_at TEXT NOT NULL
);

-- Proposals — work a cog found mid-ticket that isn't its own, held for the operator (or the
-- admiral) to accept or drop. Either a NEW ticket (the draft columns; filed on accept) or an
-- EXISTING one to take on (`existing` + `why`). Either shape lands in `convoy` on accept (filed
-- into it, or joins it) — the proposing ticket's (`from_ticket`) unless it named its own.
-- `accepted_as` = the ticket it became or was; ended without it = dropped. ended_at NULL = open.
CREATE TABLE IF NOT EXISTS proposals (
    id              INTEGER PRIMARY KEY,
    from_ticket     TEXT REFERENCES tickets(key),     -- nullable; the proposing cog's ticket
    existing        TEXT REFERENCES tickets(key),     -- an existing ticket; NULL = a new one (the draft)
    why             TEXT,                             -- an existing ticket's case for taking it on
    title           TEXT,                             -- the draft: title / kind / description / convoy / blockers
    kind            TEXT,                             -- story | bug | task | epic | … (the tracker's type)
    description     TEXT,
    convoy          TEXT,                             -- either shape: where it lands on accept
    blocked_by_json TEXT NOT NULL DEFAULT '[]',       -- ticket keys; JSON, not a child table
    created_at      TEXT NOT NULL,
    ended_at        TEXT,
    accepted_as     TEXT,
    CHECK (coalesce(CASE WHEN existing IS NULL
                THEN length(trim(title)) > 0 AND kind IS NOT NULL AND description IS NOT NULL AND why IS NULL
                ELSE length(trim(why)) > 0 AND title IS NULL AND kind IS NULL AND description IS NULL
           END, 0)),                                  -- exactly one shape (coalesce: a NULL check would pass)
    CHECK (accepted_as IS NULL OR ended_at IS NOT NULL)  -- accepted ⇒ ended
);

-- Live config: a KV mirror of fleet.toml's overridable levers (the cadences, dry_run,
-- the liveness thresholds). An ABSENT key falls through to fleet.toml/default — the
-- absence IS the precedence (defaults < toml < settings < env). fleetd reads these live
-- each tick, so `fleetc settings set` takes effect with no daemon restart. Paths/binds/
-- forge-target are deliberately NOT here — structural, bound at process start.
CREATE TABLE IF NOT EXISTS settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

-- The CDC change-log. seq = monotonic follow-cursor (never reused).
CREATE TABLE IF NOT EXISTS events (
    seq          INTEGER PRIMARY KEY AUTOINCREMENT,
    created_at   TEXT NOT NULL,
    kind         TEXT NOT NULL,                        -- 'ship.merged', 'tick.ran', …
    entity_kind  TEXT NOT NULL
                 CHECK (entity_kind IN ('session','ticket','ship','escalation','ticker','convoy','lookout')),
    entity_ref   TEXT NOT NULL,                        -- stringified id/key (polymorphic; no FK)
    payload_json TEXT NOT NULL DEFAULT '{}'
);
CREATE INDEX IF NOT EXISTS events_by_entity ON events(entity_kind, entity_ref);
