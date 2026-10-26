-- fleet ledger schema (PLAN §3.1). The single source of truth for STATE shape;
-- integrity lives here (NOT NULL / CHECK / FK), the domain type is the shape.
-- Facts are stored; status/liveness/mergeable are derived at read time.

-- The work order + the board (all sprint tickets, upsert-filled by the tracker).
CREATE TABLE IF NOT EXISTS tickets (
    key         TEXT PRIMARY KEY,                     -- '<PREFIX>-<n>', e.g. 'PROJ-1234'
    convoy      TEXT,                                 -- nullable; grouping key, ticket-key shaped (NOT a ticket)
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
    kind               TEXT NOT NULL CHECK (kind IN ('worker','sailor','hand','captain','lookout')),
    ticket             TEXT REFERENCES tickets(key),    -- nullable; worker/sailor require one (CHECK below)
    convoy             TEXT,                             -- nullable; a captain commands one (CHECK below)
    worktree_path      TEXT NOT NULL,
    tmux_session       TEXT NOT NULL,
    harness_session_id TEXT,
    created_at         TEXT NOT NULL,
    ended_at           TEXT,
    anchored_at        TEXT,                            -- operator anchor (stop & hold); NULL = under way
    -- a worker/sailor is bound to a ticket; a captain/hand/lookout/singleton may be ticketless.
    CHECK (ticket IS NOT NULL OR kind NOT IN ('worker','sailor')),
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
-- ...and ONE live lookout for the whole factory (the singleton allied-ship reviewer).
CREATE UNIQUE INDEX IF NOT EXISTS one_live_lookout
    ON sessions(kind) WHERE kind = 'lookout' AND ended_at IS NULL;

-- Convoys — the operator-curated grouping a captain commands. Existence is an operator
-- fact (open → active); `ended_at` is the captain's "all delivered" close, which reconcile
-- reads to reap the captain. Not FK'd to tickets: a convoy key is adapter-neutral.
CREATE TABLE IF NOT EXISTS convoys (
    key         TEXT PRIMARY KEY,
    title       TEXT,
    created_at  TEXT NOT NULL,
    ended_at    TEXT,                                 -- NULL = active
    anchored_at TEXT                                  -- operator anchor (hold the captain); NULL = under way
);

-- The singleton lookout's own control row. The allied-ship review cog is ticketless + convoyless, so
-- it has no existing row to hang a hold on — this one-row table IS its home. `anchored_at` =
-- operator anchor on the review loop (NULL = under way), which reconcile's lookout launch gate
-- reads. One row, seeded at init; room to grow (future review toggles/state).
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
    reviewed   TEXT                                    -- head_sha the last lookout reviewed (review cold-loop change-detector)
);

-- The churny forge mirror, 1:1 with a ship (PK=FK enforces one snapshot per ship).
CREATE TABLE IF NOT EXISTS ship_snapshots (
    ship_id            INTEGER PRIMARY KEY REFERENCES ships(id),
    title              TEXT    NOT NULL,
    state              TEXT    NOT NULL CHECK (state IN ('draft','open','merged','closed')),
    head_sha           TEXT    NOT NULL,
    has_conflicts      INTEGER NOT NULL,               -- 0/1
    open_threads       INTEGER NOT NULL,
    approvals_current  INTEGER NOT NULL,
    approvals_required INTEGER NOT NULL,
    armed              INTEGER NOT NULL,               -- 0/1
    on_merge_train     INTEGER NOT NULL,               -- 0/1
    pipeline           TEXT    CHECK (pipeline IN ('running','success','failed','canceled','manual')),
    merge_status       TEXT,                           -- GitLab detailed_merge_status (open vocab; no CHECK)
    synced_at          TEXT    NOT NULL
);

-- The dispatch queue (operational intent). One active order per ticket.
CREATE TABLE IF NOT EXISTS orders (
    ticket      TEXT PRIMARY KEY REFERENCES tickets(key),
    auto_impl   INTEGER NOT NULL DEFAULT 1,            -- 0/1 (default worker)
    auto_merge  INTEGER NOT NULL DEFAULT 1,            -- 0/1 (0 = the sailor waits for the operator to merge)
    fork_from   TEXT NOT NULL,
    ac_summary  TEXT NOT NULL,
    park_at     TEXT,
    worktree_path TEXT,                            -- materialised by the producer; reconcile reads it at spawn, never creates it
    created_at  TEXT NOT NULL
);

-- Escalations. ended_at NULL = open; resolution folded in.
CREATE TABLE IF NOT EXISTS escalations (
    id           INTEGER PRIMARY KEY,
    ticket       TEXT NOT NULL REFERENCES tickets(key),
    raised_by    TEXT NOT NULL CHECK (raised_by IN ('worker','sailor','hand')),
    summary      TEXT NOT NULL,
    evidence_json TEXT NOT NULL DEFAULT '[]',          -- opaque payload; JSON, not a child table
    ship_iid     INTEGER,
    phase        TEXT,
    created_at   TEXT NOT NULL,
    ended_at     TEXT,
    resolution   TEXT
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
