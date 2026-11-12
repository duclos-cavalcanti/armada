//! fleet ledger — the SQLite persistence adapter (PLAN §3.1).
//!
//! `fleetd` is the single writer. This crate owns the schema (the integrity layer:
//! NOT NULL / CHECK / FK) and the row↔domain mapping behind the repository ports.
//! `domain` stays sqlite-free; the enum ↔ TEXT mapping uses `domain`'s `as_str`/
//! `parse` helpers.

use anyhow::Result;
use domain::ports::{
    CaseRepo, ConvoyRepo, EventLog, InspectionRepo, LookoutRepo, MemoryRepo, OrderRepo, ProposalRepo, ReportRepo,
    SessionRepo, ShipRepo, TicketRepo,
};
use domain::*;
use rusqlite::{params, Connection, OptionalExtension};

/// The DDL — the single source of truth for STATE shape (see `schema.sql`).
pub const SCHEMA: &str = include_str!("schema.sql");

/// The telemetry store — a SEPARATE SQLite DB from the operational ledger (append-heavy
/// cost/health, kept off the single-writer path). See `telemetry.rs`.
pub mod telemetry;

/// The ledger — a single-writer SQLite connection with the schema applied.
pub struct Ledger {
    conn: Connection,
}

impl Ledger {
    /// Open (or create) the ledger at `path`: WAL mode, FKs on, schema applied.
    pub fn open(path: &str) -> Result<Self> {
        let conn = Connection::open(path)?;
        init(&conn)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        Ok(Self { conn })
    }

    /// Open the ledger READ-ONLY — a concurrent reader (e.g. `fleetc plan`) beside fleetd's single
    /// writer. No schema init, no pragmas: the writer owns the file; this connection only reads.
    pub fn open_readonly(path: &str) -> Result<Self> {
        let conn = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        Ok(Self { conn })
    }

    /// The most recent reconcile `TickRan` event — fleetd's last plan (dry-run: `planned[]`; live:
    /// `applied[]`/`failed[]`). `None` if no tick has run yet. The read behind `fleetc plan`.
    pub fn latest_tick(&self) -> Result<Option<Event>> {
        Ok(self
            .conn
            .query_row(
                "SELECT * FROM events WHERE kind = ?1 ORDER BY seq DESC LIMIT 1",
                params![EventKind::TickRan.as_str()],
                row_to_event,
            )
            .optional()?)
    }

    /// Every event of `kind`, oldest first.
    pub fn events_of(&self, kind: EventKind) -> Result<Vec<Event>> {
        let mut stmt = self.conn.prepare("SELECT * FROM events WHERE kind = ?1 ORDER BY seq")?;
        let rows = stmt.query_map(params![kind.as_str()], row_to_event)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The most recent health check fleetd recorded (`payload.ok`, `payload.problems`): what the
    /// dashboard's `fleet:` light shows. `None` before the first check.
    pub fn latest_health(&self) -> Result<Option<Event>> {
        Ok(self
            .conn
            .query_row(
                "SELECT * FROM events WHERE kind = ?1 ORDER BY seq DESC LIMIT 1",
                params![EventKind::HealthChecked.as_str()],
                row_to_event,
            )
            .optional()?)
    }

    /// When fleetd last synced (the latest `tick_ran` of the `sync` ticker): the health check runs
    /// right after it. `None` before the first sync.
    pub fn last_sync(&self) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT created_at FROM events WHERE kind = ?1 AND entity_ref = 'sync' ORDER BY seq DESC LIMIT 1",
                params![EventKind::TickRan.as_str()],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// The convoy's plan history, oldest first: each `ConvoyPlanned` (`payload.plan`: the plan as
    /// set; `null`: cleared). The read behind `fleetc convoy plan`.
    pub fn plan_history(&self, convoy: &ConvoyKey) -> Result<Vec<Event>> {
        let mut stmt = self.conn.prepare(
            "SELECT * FROM events WHERE kind = ?1 AND entity_kind = ?2 AND entity_ref = ?3 ORDER BY seq",
        )?;
        let rows = stmt.query_map(
            params![EventKind::ConvoyPlanned.as_str(), EntityKind::Convoy.as_str(), convoy.0],
            row_to_event,
        )?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// How many lookouts have been launched on the ship: the review round it's on.
    pub fn lookouts_launched(&self, ship: &ShipId) -> Result<u64> {
        let n: i64 = self.conn.query_row(
            "SELECT count(*) FROM sessions WHERE kind = 'lookout' AND ship = ?1",
            params![ship.0],
            |r| r.get(0),
        )?;
        Ok(n as u64)
    }

    /// When the convoy's latest captain was launched (`None`: never). A captain runs cold, one pass
    /// per launch, so this is how recently its convoy was looked at.
    pub fn last_captain_launch(&self, convoy: &ConvoyKey) -> Result<Option<String>> {
        self.latest_captain("created_at", convoy)
    }

    /// Each convoy's inspection verdicts so far: `(convoy, refuted, confirmed)`.
    pub fn verdicts(&self) -> Result<Vec<(ConvoyKey, i64, i64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT coalesce(m.convoy, t.convoy) AS convoy,
                    sum(i.verdict = 'refute'), sum(i.verdict = 'confirm')
             FROM inspections i JOIN sessions s ON s.id = i.session_id JOIN tickets t ON t.key = s.ticket
             LEFT JOIN convoy_members m ON m.ticket = t.key
             WHERE coalesce(m.convoy, t.convoy) IS NOT NULL GROUP BY 1",
        )?;
        let rows = stmt.query_map([], |r| Ok((ConvoyKey(r.get(0)?), r.get(1)?, r.get(2)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// When the convoy's latest captain finished (`None`: none has) — its rest is counted from here.
    pub fn last_captain_finish(&self, convoy: &ConvoyKey) -> Result<Option<String>> {
        self.latest_captain("ended_at", convoy)
    }

    fn latest_captain(&self, column: &str, convoy: &ConvoyKey) -> Result<Option<String>> {
        Ok(self.conn.query_row(
            &format!("SELECT max({column}) FROM sessions WHERE kind = 'captain' AND convoy = ?1"),
            params![convoy.0],
            |r| r.get(0),
        )?)
    }

    /// Why a ticket is held: the reason on its latest `TicketHeld` event (`""` when the hold gave
    /// none; `None` when it was never held). The hold itself is the ticket's `held` flag.
    pub fn held_reason(&self, key: &TicketKey) -> Result<Option<String>> {
        let payload: Option<String> = self
            .conn
            .query_row(
                "SELECT payload_json FROM events WHERE kind = ?1 AND entity_ref = ?2 ORDER BY seq DESC LIMIT 1",
                params![EventKind::TicketHeld.as_str(), key.0],
                |r| r.get(0),
            )
            .optional()?;
        Ok(payload.map(|p| {
            serde_json::from_str::<serde_json::Value>(&p)
                .ok()
                .and_then(|v| v.get("reason").and_then(|r| r.as_str()).map(String::from))
                .unwrap_or_default()
        }))
    }

    /// An in-memory ledger with the schema applied — for tests.
    pub fn in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        init(&conn)?;
        Ok(Self { conn })
    }

    /// Total row changes on this connection since it opened — the tick version-span:
    /// if it doesn't move across a tick, the tick wrote nothing (no-op detection).
    pub fn total_changes(&self) -> u64 {
        self.conn.total_changes()
    }

    /// The convoy numbered `no` (`#N`), open or closed; `None` = no convoy has that number.
    pub fn convoy_by_no(&self, no: i64) -> Result<Option<ConvoyKey>> {
        let key: Option<String> =
            self.conn.query_row("SELECT key FROM convoys WHERE no = ?1", params![no], |r| r.get(0)).optional()?;
        Ok(key.map(ConvoyKey))
    }

    // ── settings (the live-config KV; see `schema.sql` and fleetd's `settings` module) ──

    /// Read a live-config value. `None` = absent → the caller falls through to its base
    /// (fleet.toml/default). A general KV read: any key, not just the known levers.
    pub fn setting_get(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row("SELECT value FROM settings WHERE key = ?1", params![key], |r| r.get(0))
            .optional()?)
    }

    /// Overwrite a live-config value (upsert). Validation (known lever + parseable) is the
    /// writer's job before this call; the table itself is a general string KV.
    pub fn setting_set(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO settings(key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }
}

fn init(conn: &Connection) -> Result<()> {
    migrate(conn)?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.execute_batch(SCHEMA)?;
    stamp(conn, SCHEMA_VERSION, "ledger")
}

/// The steps that bring an older ledger up to `schema.sql`, in order: `MIGRATIONS[i]` takes v`i+1`
/// to v`i+2`. When an existing table changes shape, change it in `schema.sql` (what a fresh ledger
/// is built from) AND append its step here. A new table needs neither: `CREATE TABLE IF NOT
/// EXISTS` adds it to an older ledger the next time fleetd opens it.
const MIGRATIONS: &[&str] = &[
    // v2: an MR's total review threads, beside its open ones.
    "ALTER TABLE ship_snapshots ADD COLUMN total_threads INTEGER NOT NULL DEFAULT 0;",
    // v3: the MRs declared as blocking it, so reconcile asks the forge only when they drift.
    "ALTER TABLE ship_snapshots ADD COLUMN blocked_by_json TEXT NOT NULL DEFAULT '[]';",
    // v4: escalations were never written (an escalation is a report); inspections judge them.
    "DROP TABLE IF EXISTS escalations;",
    // v5: an admiral session, convened on a confirmed escalation. SQLite can't alter a CHECK, so the
    // table is rebuilt (its indexes come back with schema.sql).
    "CREATE TABLE sessions_v5 (
        id                 INTEGER PRIMARY KEY,
        kind               TEXT NOT NULL CHECK (kind IN ('worker','sailor','hand','captain','lookout','admiral')),
        ticket             TEXT REFERENCES tickets(key),
        convoy             TEXT,
        worktree_path      TEXT NOT NULL,
        tmux_session       TEXT NOT NULL,
        harness_session_id TEXT,
        created_at         TEXT NOT NULL,
        ended_at           TEXT,
        anchored_at        TEXT,
        CHECK (ticket IS NOT NULL OR kind NOT IN ('worker','sailor','admiral')),
        CHECK (kind <> 'captain' OR convoy IS NOT NULL)
    );
    INSERT INTO sessions_v5 (id, kind, ticket, convoy, worktree_path, tmux_session, harness_session_id, created_at, ended_at, anchored_at)
        SELECT id, kind, ticket, convoy, worktree_path, tmux_session, harness_session_id, created_at, ended_at, anchored_at FROM sessions;
    DROP TABLE sessions;
    ALTER TABLE sessions_v5 RENAME TO sessions;",
    // v6: a lookout per allied ship, up to a cap, in place of the singleton.
    "ALTER TABLE sessions ADD COLUMN ship INTEGER REFERENCES ships(id);
    DROP INDEX IF EXISTS one_live_lookout;",
    // v7: a ship's review rounds (sailor sweeps), so its sailor escalates at 10.
    "ALTER TABLE ships ADD COLUMN rounds INTEGER NOT NULL DEFAULT 0;",
    // v8: a convoy's plan keeps its history (ConvoyPlanned events); each standing plan seeds it.
    "INSERT INTO events(created_at, kind, entity_kind, entity_ref, payload_json)
        SELECT updated_at, 'convoy_planned', 'convoy', convoy, json_object('plan', body) FROM convoy_plans ORDER BY updated_at;",
    // v9: escalation cases. The escalations judged so far become cases (one per session and
    // question, its verdicts attached), and so do the ones standing now: precedent from the start.
    // A past case settles when its session ended, else at its last verdict.
    "CREATE TABLE IF NOT EXISTS cases (
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
    ALTER TABLE inspections ADD COLUMN case_id INTEGER REFERENCES cases(id);
    CREATE TEMP TABLE judged AS
        SELECT i.id, i.session_id, i.reported_at, TRIM(COALESCE((SELECT json_extract(e.payload_json, '$.last_action')
            FROM events e WHERE e.kind = 'status_reported' AND e.entity_ref = CAST(i.session_id AS TEXT)
            AND e.created_at = i.reported_at LIMIT 1), '')) AS question
        FROM inspections i;
    INSERT INTO cases(ticket, convoy, session_id, question, raised_at, reports)
        SELECT s.ticket, t.convoy, s.id, j.question, MIN(j.reported_at), COUNT(DISTINCT j.reported_at)
        FROM judged j JOIN sessions s ON s.id = j.session_id LEFT JOIN tickets t ON t.key = s.ticket
        WHERE s.ticket IS NOT NULL GROUP BY s.id, j.question ORDER BY MIN(j.reported_at);
    INSERT INTO cases(ticket, convoy, session_id, question, raised_at)
        SELECT s.ticket, t.convoy, s.id, TRIM(COALESCE(r.last_action, '')), r.reported_at
        FROM reports r JOIN sessions s ON s.id = r.session_id LEFT JOIN tickets t ON t.key = s.ticket
        WHERE r.state = 'escalated' AND s.ended_at IS NULL AND s.kind IN ('worker','sailor') AND s.ticket IS NOT NULL
          AND NOT EXISTS (SELECT 1 FROM cases c WHERE c.session_id = s.id AND c.question = TRIM(COALESCE(r.last_action, '')));
    UPDATE inspections SET case_id = (SELECT c.id FROM judged j JOIN cases c ON c.session_id = j.session_id AND c.question = j.question
        WHERE j.id = inspections.id);
    UPDATE cases SET settled_at = COALESCE((SELECT ended_at FROM sessions WHERE id = cases.session_id),
            (SELECT MAX(inspected_at) FROM inspections WHERE case_id = cases.id), raised_at)
        WHERE NOT EXISTS (SELECT 1 FROM reports r JOIN sessions s ON s.id = r.session_id
            WHERE r.session_id = cases.session_id AND s.ended_at IS NULL AND r.state = 'escalated'
              AND TRIM(COALESCE(r.last_action, '')) = cases.question);
    DROP TABLE temp.judged;",
    // v10: when an MR's review threads last moved, so a sailor waits for a reviewer to finish.
    "ALTER TABLE ship_snapshots ADD COLUMN threads_changed_at TEXT;",
    // v11: each convoy's short number (#N), the standing ones numbered in the order they opened.
    "ALTER TABLE convoys ADD COLUMN no INTEGER;
    UPDATE convoys SET no = (SELECT COUNT(*) FROM convoys c
        WHERE c.created_at < convoys.created_at OR (c.created_at = convoys.created_at AND c.key <= convoys.key));",
    // v12: each convoy's goal, the ticket whose Done proves its outcome (its critical path's end).
    "ALTER TABLE convoys ADD COLUMN goal TEXT REFERENCES tickets(key);",
    // v13: standing convoys, fleet-wide work never delivered.
    "ALTER TABLE convoys ADD COLUMN standing INTEGER NOT NULL DEFAULT 0 CHECK (standing IN (0, 1));",
];

/// The ledger's schema version (SQLite `user_version`): one past its last migration.
pub const SCHEMA_VERSION: i64 = 1 + MIGRATIONS.len() as i64;

/// Run the migrations an older ledger is missing, in one transaction, and stamp it current. A fresh
/// ledger (`0`) has nothing to migrate; one newer than this build is left for `stamp` to refuse.
/// Runs with foreign keys off (SQLite's procedure for rebuilding a table), so it checks them itself
/// before committing.
fn migrate(conn: &Connection) -> Result<()> {
    let found: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    if found == 0 || found >= SCHEMA_VERSION {
        return Ok(());
    }
    conn.pragma_update(None, "foreign_keys", "OFF")?; // a no-op inside a transaction: set it first
    let tx = conn.unchecked_transaction()?;
    for step in &MIGRATIONS[found as usize - 1..] {
        tx.execute_batch(step)?;
    }
    let dangling: i64 = tx.query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |r| r.get(0))?;
    anyhow::ensure!(dangling == 0, "migration left {dangling} dangling foreign key(s)");
    tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    tx.commit()?;
    Ok(())
}

/// Stamp a fresh database with `version`; refuse one stamped with another. (`0` = never stamped.)
pub(crate) fn stamp(conn: &Connection, version: i64, what: &str) -> Result<()> {
    let found: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    match found {
        0 => Ok(conn.pragma_update(None, "user_version", version)?),
        v if v == version => Ok(()),
        v => anyhow::bail!("{what} schema v{v}, but this build expects v{version} — no migrations: start fresh or use a matching build"),
    }
}

/// A database file's schema version (`user_version`), read-only — what `fleetc import` checks.
pub fn schema_version(path: &str) -> Result<i64> {
    let conn = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    Ok(conn.pragma_query_value(None, "user_version", |r| r.get(0))?)
}

/// Snapshot a live database (the ledger or telemetry) into `to`: `VACUUM INTO` writes a
/// consistent, compacted, standalone copy (no WAL beside it) from a read-only connection, so it
/// runs beside fleetd's writer. `to` must not exist yet.
pub fn snapshot(from: &str, to: &str) -> Result<()> {
    let conn = Connection::open_with_flags(from, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    conn.execute("VACUUM INTO ?1", params![to])?;
    Ok(())
}

/// Map an `Option<T>` (a failed enum parse) to a rusqlite conversion error. The DB
/// CHECK constraints make this unreachable in practice, but we fail loud, not silent.
fn require<T>(o: Option<T>, col: usize, what: &str) -> rusqlite::Result<T> {
    o.ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            col,
            rusqlite::types::Type::Text,
            format!("invalid {what} in ledger").into(),
        )
    })
}

/// Re-check a stored ticket key's `<PREFIX>-<n>` shape on read — never silently coerced.
fn ticket_key(s: String) -> rusqlite::Result<TicketKey> {
    require(TicketKey::parse(&s).ok(), 0, "ticket key")
}

/// Re-check a stored convoy key on read — same shape as a ticket key.
fn convoy_key(s: String) -> rusqlite::Result<ConvoyKey> {
    require(ConvoyKey::parse(&s).ok(), 0, "convoy key")
}

// ── tickets ─────────────────────────────────────────────────────────────────────

fn row_to_ticket(r: &rusqlite::Row) -> rusqlite::Result<Ticket> {
    let status: Option<String> = r.get("status")?;
    let status = match status {
        Some(s) => Some(require(TicketStatus::parse(&s), 6, "ticket status")?),
        None => None,
    };
    Ok(Ticket {
        key: ticket_key(r.get("key")?)?,
        convoy: r.get::<_, Option<String>>("convoy")?.map(convoy_key).transpose()?,
        held: r.get::<_, i64>("held")? != 0,
        created_at: r.get("created_at")?,
        blocked_by: Vec::new(), // filled by the caller from ticket_blocks
        title: r.get("title")?,
        assignee: r.get("assignee")?,
        status,
        synced_at: r.get("synced_at")?,
    })
}

/// A ticket read: every `tickets` column, its convoy resolved — the one it was added to
/// (`convoy_members`), else its epic's. Every reader sees the one convoy the fleet drives it in.
const TICKET_READ: &str = "SELECT t.key, coalesce(m.convoy, t.convoy) AS convoy, t.held, t.created_at, t.title, \
     t.assignee, t.status, t.synced_at FROM tickets t LEFT JOIN convoy_members m ON m.ticket = t.key";

impl Ledger {
    fn blockers_of(&self, key: &TicketKey) -> Result<Vec<TicketKey>> {
        let mut stmt = self
            .conn
            .prepare("SELECT blocked_by FROM ticket_blocks WHERE ticket = ?1 ORDER BY blocked_by")?;
        let rows = stmt.query_map(params![key.0], |r| ticket_key(r.get(0)?))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }
}

impl TicketRepo for Ledger {
    fn upsert(&self, t: &Ticket) -> Result<()> {
        self.conn.execute(
            "INSERT INTO tickets(key, convoy, held, created_at, title, assignee, status, synced_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(key) DO UPDATE SET
               convoy   = excluded.convoy,
               held     = excluded.held,
               title    = excluded.title,
               assignee = excluded.assignee,
               status   = excluded.status,
               synced_at= excluded.synced_at",
            params![
                t.key.0,
                t.convoy.as_ref().map(|k| &k.0),
                t.held as i64,
                t.created_at,
                t.title,
                t.assignee,
                t.status.map(|s| s.as_str()),
                t.synced_at,
            ],
        )?;
        Ok(())
    }

    fn get(&self, key: &TicketKey) -> Result<Option<Ticket>> {
        let mut t = self
            .conn
            .query_row(&format!("{TICKET_READ} WHERE t.key = ?1"), params![key.0], row_to_ticket)
            .optional()?;
        if let Some(ref mut ticket) = t {
            ticket.blocked_by = self.blockers_of(key)?;
        }
        Ok(t)
    }

    fn board(&self) -> Result<Vec<Ticket>> {
        let mut stmt = self.conn.prepare(&format!("{TICKET_READ} ORDER BY t.key"))?;
        let rows = stmt.query_map([], row_to_ticket)?;
        let mut tickets: Vec<Ticket> = rows.collect::<rusqlite::Result<_>>()?;
        for t in &mut tickets {
            t.blocked_by = self.blockers_of(&t.key)?;
        }
        Ok(tickets)
    }

    fn set_blocked_by(&self, key: &TicketKey, blockers: &[TicketKey]) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute("DELETE FROM ticket_blocks WHERE ticket = ?1", params![key.0])?;
        for b in blockers {
            // Skip a blocker we don't track (out-of-sprint, or not yet synced this pass) rather than
            // trip the FK — the edge wires itself on a later sync once both tickets are in the ledger
            // (the sync loop is convergent). The schema's FK still guards against a truly dangling edge.
            tx.execute(
                "INSERT INTO ticket_blocks(ticket, blocked_by)
                 SELECT ?1, ?2 WHERE EXISTS (SELECT 1 FROM tickets WHERE key = ?2)",
                params![key.0, b.0],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    fn set_hand(&self, key: &TicketKey, hand: bool, at: &str) -> Result<()> {
        if hand {
            self.conn.execute(
                "INSERT INTO hand_tickets(ticket, marked_at) VALUES (?1, ?2) ON CONFLICT(ticket) DO NOTHING",
                params![key.0, at],
            )?;
        } else {
            self.conn.execute("DELETE FROM hand_tickets WHERE ticket = ?1", params![key.0])?;
        }
        Ok(())
    }

    fn is_hand(&self, key: &TicketKey) -> Result<bool> {
        Ok(self.conn.query_row("SELECT EXISTS (SELECT 1 FROM hand_tickets WHERE ticket = ?1)", params![key.0], |r| r.get(0))?)
    }

    fn set_held(&self, key: &TicketKey, held: bool, reason: Option<&str>, at: &str) -> Result<()> {
        // Flip held + log TicketHeld(reason)/TicketReleased (CDC, same txn). Only a real change
        // (held != current) touches a row; a no-op (already in that state) logs nothing.
        let tx = self.conn.unchecked_transaction()?;
        let n = tx.execute(
            "UPDATE tickets SET held = ?2 WHERE key = ?1 AND held != ?2",
            params![key.0, held as i64],
        )?;
        if n > 0 {
            let (kind, payload) = if held {
                (EventKind::TicketHeld, serde_json::json!({ "reason": reason }))
            } else {
                (EventKind::TicketReleased, serde_json::json!({}))
            };
            tx.execute(
                "INSERT INTO events(created_at, kind, entity_kind, entity_ref, payload_json)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![at, kind.as_str(), EntityKind::Ticket.as_str(), key.0, serde_json::to_string(&payload)?],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    fn sync(&self, t: &TrackerTicket, now: &str) -> Result<()> {
        // Insert with defaults (held=0, created_at=now) if new; on conflict update ONLY
        // the tracker-sourced cache fields — `held` and `created_at` stay ours.
        self.conn.execute(
            "INSERT INTO tickets(key, convoy, held, created_at, title, assignee, status, synced_at)
             VALUES (?1, ?2, 0, ?3, ?4, ?5, ?6, ?3)
             ON CONFLICT(key) DO UPDATE SET
               convoy    = excluded.convoy,
               title     = excluded.title,
               assignee  = excluded.assignee,
               status    = excluded.status,
               synced_at = excluded.synced_at",
            params![
                t.key.0,
                t.convoy.as_ref().map(|k| &k.0),
                now,
                t.title,
                t.assignee,
                t.status.as_str(),
            ],
        )?;
        // Jira is authoritative for the edge set: always re-write it, so an emptied `blocked_by`
        // (a blocker removed/unlinked in Jira) clears the stale edges instead of lingering.
        self.set_blocked_by(&t.key, &t.blocked_by)?;
        if t.has(TicketLabel::ProductOwner) {
            self.set_hand(&t.key, true, now)?;
        }
        Ok(())
    }
}

// ── sessions ────────────────────────────────────────────────────────────────────

fn row_to_session(r: &rusqlite::Row) -> rusqlite::Result<Session> {
    let kind: String = r.get("kind")?;
    Ok(Session {
        id: SessionId(r.get("id")?),
        kind: require(CrewKind::parse(&kind), 1, "session kind")?,
        ticket: r.get::<_, Option<String>>("ticket")?.map(ticket_key).transpose()?,
        convoy: r.get::<_, Option<String>>("convoy")?.map(convoy_key).transpose()?,
        worktree_path: r.get("worktree_path")?,
        tmux_session: r.get("tmux_session")?,
        harness_session_id: r.get("harness_session_id")?,
        created_at: r.get("created_at")?,
        ended_at: r.get("ended_at")?,
        anchored_at: r.get("anchored_at")?,
        ship: r.get::<_, Option<i64>>("ship")?.map(ShipId),
    })
}

impl SessionRepo for Ledger {
    fn start(&self, s: &Session) -> Result<Option<SessionId>> {
        let res = self.conn.execute(
            "INSERT INTO sessions(kind, ticket, convoy, worktree_path, tmux_session, harness_session_id, created_at, ended_at, anchored_at, ship)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                s.kind.as_str(),
                s.ticket.as_ref().map(|k| &k.0),
                s.convoy.as_ref().map(|k| &k.0),
                s.worktree_path,
                s.tmux_session,
                s.harness_session_id,
                s.created_at,
                s.ended_at,
                s.anchored_at,
                s.ship.as_ref().map(|id| id.0),
            ],
        );
        match res {
            Ok(_) => Ok(Some(SessionId(self.conn.last_insert_rowid()))),
            // A live-spawn lock (one per ticket + kind, per convoy, per lookout's ship)
            // rejected a duplicate live cog: a benign skip (the slot is already held), NOT
            // an error. An FK, a CHECK, or any other constraint still propagates.
            Err(rusqlite::Error::SqliteFailure(e, _))
                if e.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE =>
            {
                Ok(None)
            }
            Err(e) => Err(e.into()),
        }
    }

    fn get(&self, id: &SessionId) -> Result<Option<Session>> {
        Ok(self
            .conn
            .query_row("SELECT * FROM sessions WHERE id = ?1", params![id.0], row_to_session)
            .optional()?)
    }

    fn for_ticket(&self, key: &TicketKey) -> Result<Vec<Session>> {
        let mut stmt = self
            .conn
            .prepare("SELECT * FROM sessions WHERE ticket = ?1 ORDER BY id")?;
        let rows = stmt.query_map(params![key.0], row_to_session)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    fn live(&self) -> Result<Vec<Session>> {
        let mut stmt = self
            .conn
            .prepare("SELECT * FROM sessions WHERE ended_at IS NULL ORDER BY id")?;
        let rows = stmt.query_map([], row_to_session)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    fn end(&self, id: &SessionId, at: &str) -> Result<()> {
        // Its open case settles with it: no cog stands on it any more.
        let tx = self.conn.unchecked_transaction()?;
        tx.execute("UPDATE sessions SET ended_at = ?2 WHERE id = ?1", params![id.0, at])?;
        tx.execute("UPDATE cases SET settled_at = ?2 WHERE session_id = ?1 AND settled_at IS NULL", params![id.0, at])?;
        tx.commit()?;
        Ok(())
    }

    fn anchor(&self, id: &SessionId, at: &str) -> Result<()> {
        // Set anchored_at + SessionAnchored (CDC, same txn). Only a live, not-yet-anchored
        // session drops anchor; a no-op logs nothing.
        let tx = self.conn.unchecked_transaction()?;
        let n = tx.execute(
            "UPDATE sessions SET anchored_at = ?2 WHERE id = ?1 AND anchored_at IS NULL AND ended_at IS NULL",
            params![id.0, at],
        )?;
        if n > 0 {
            tx.execute(
                "INSERT INTO events(created_at, kind, entity_kind, entity_ref, payload_json)
                 VALUES (?1, ?2, ?3, ?4, '{}')",
                params![at, EventKind::SessionAnchored.as_str(), EntityKind::Session.as_str(), id.0.to_string()],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    fn weigh(&self, id: &SessionId, at: &str) -> Result<()> {
        // Clear anchored_at + SessionWeighed (CDC, same txn). A no-op (not anchored) logs nothing.
        let tx = self.conn.unchecked_transaction()?;
        let n = tx.execute(
            "UPDATE sessions SET anchored_at = NULL WHERE id = ?1 AND anchored_at IS NOT NULL",
            params![id.0],
        )?;
        if n > 0 {
            tx.execute(
                "INSERT INTO events(created_at, kind, entity_kind, entity_ref, payload_json)
                 VALUES (?1, ?2, ?3, ?4, '{}')",
                params![at, EventKind::SessionWeighed.as_str(), EntityKind::Session.as_str(), id.0.to_string()],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
}

// ── reports (a session's latest self-report) ─────────────────────────────────────

/// Widen a report row back to a typed `Report`. The joined `kind` selects the cog's
/// state vocabulary — a stored string invalid for that cog fails loud via `require`.
fn row_to_report(r: &rusqlite::Row) -> rusqlite::Result<Report> {
    let kind: String = r.get("kind")?;
    let kind = require(CrewKind::parse(&kind), 1, "session kind")?;
    let state: String = r.get("state")?;
    Ok(Report {
        session: SessionId(r.get("session_id")?),
        state: require(CogState::parse(kind, &state), 2, "report state")?,
        last_action: r.get("last_action")?,
        reported_at: r.get("reported_at")?,
    })
}

impl ReportRepo for Ledger {
    fn record(&self, r: &Report) -> Result<()> {
        // Latest-row upsert + history event in one txn — they never diverge.
        let payload =
            serde_json::json!({ "state": r.state.as_str(), "last_action": r.last_action });
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO reports(session_id, state, last_action, reported_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(session_id) DO UPDATE SET
               state=excluded.state, last_action=excluded.last_action, reported_at=excluded.reported_at",
            params![r.session.0, r.state.as_str(), r.last_action, r.reported_at],
        )?;
        tx.execute(
            "INSERT INTO events(created_at, kind, entity_kind, entity_ref, payload_json)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                r.reported_at,
                EventKind::StatusReported.as_str(),
                EntityKind::Session.as_str(),
                r.session.0.to_string(),
                serde_json::to_string(&payload)?,
            ],
        )?;
        track_case(&tx, r)?;
        tx.commit()?;
        Ok(())
    }

    fn latest(&self, session: &SessionId) -> Result<Option<Report>> {
        Ok(self
            .conn
            .query_row(
                "SELECT r.session_id, s.kind, r.state, r.last_action, r.reported_at
                 FROM reports r JOIN sessions s ON s.id = r.session_id
                 WHERE r.session_id = ?1",
                params![session.0],
                row_to_report,
            )
            .optional()?)
    }
}

/// Keep the session's case in step with its report, in the report's txn. A worker's or sailor's
/// escalation opens a case. Until the case is answered (a REFUTE's way out, or a ruling) its cog
/// waits on it, so another escalation joins it, whatever its wording (a restart's re-report).
/// Once answered, another escalation means the answer didn't hold: a new case. Any other report
/// settles it.
fn track_case(tx: &Connection, r: &Report) -> Result<()> {
    if !matches!(r.state, CogState::Worker(_) | CogState::Sailor(_)) {
        return Ok(());
    }
    let question = r.last_action.as_deref().unwrap_or("").trim();
    let open: Option<(i64, bool)> = tx
        .query_row(
            "SELECT c.id, c.ruling IS NOT NULL
                 OR (SELECT verdict FROM inspections WHERE case_id = c.id ORDER BY id DESC LIMIT 1) IS 'refute'
             FROM cases c WHERE c.session_id = ?1 AND c.settled_at IS NULL",
            params![r.session.0],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if let Some((id, answered)) = open {
        if r.state.is_escalated() && !answered {
            tx.execute("UPDATE cases SET reports = reports + 1 WHERE id = ?1", params![id])?;
            return Ok(());
        }
        tx.execute("UPDATE cases SET settled_at = ?2 WHERE id = ?1", params![id, r.reported_at])?;
    }
    if r.state.is_escalated() {
        tx.execute(
            "INSERT INTO cases(ticket, convoy, session_id, question, raised_at)
             SELECT s.ticket, t.convoy, s.id, ?2, ?3 FROM sessions s LEFT JOIN tickets t ON t.key = s.ticket
             WHERE s.id = ?1 AND s.ticket IS NOT NULL",
            params![r.session.0, question, r.reported_at],
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_ledger_is_stamped_and_another_version_is_refused() {
        let dir = std::env::temp_dir().join(format!("ledger-version-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("ledger.db").to_string_lossy().into_owned();
        drop(Ledger::open(&path).unwrap());
        assert_eq!(schema_version(&path).unwrap(), SCHEMA_VERSION, "stamped on first open");

        let conn = Connection::open(&path).unwrap();
        conn.pragma_update(None, "user_version", SCHEMA_VERSION + 1).unwrap();
        drop(conn);
        let e = Ledger::open(&path).err().unwrap().to_string();
        let (newer, current) = (format!("schema v{}", SCHEMA_VERSION + 1), format!("expects v{SCHEMA_VERSION}"));
        assert!(e.contains(&newer) && e.contains(&current), "{e}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_older_ledger_is_migrated_in_place() {
        // A v1 ledger: ship_snapshots without total_threads.
        let dir = std::env::temp_dir().join(format!("ledger-migrate-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("ledger.db").to_string_lossy().into_owned();
        drop(Ledger::open(&path).unwrap());
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "ALTER TABLE ship_snapshots DROP COLUMN total_threads; ALTER TABLE ship_snapshots DROP COLUMN blocked_by_json; \
             ALTER TABLE ships DROP COLUMN rounds; ALTER TABLE inspections DROP COLUMN case_id; ALTER TABLE ship_snapshots DROP COLUMN threads_changed_at; DROP INDEX convoy_no; ALTER TABLE convoys DROP COLUMN no; ALTER TABLE convoys DROP COLUMN goal; ALTER TABLE convoys DROP COLUMN standing; PRAGMA user_version = 1;",
        )
        .unwrap();
        drop(conn);

        drop(Ledger::open(&path).unwrap());
        assert_eq!(schema_version(&path).unwrap(), SCHEMA_VERSION, "stamped current");
        let conn = Connection::open(&path).unwrap();
        let has: bool = conn
            .query_row("SELECT EXISTS (SELECT 1 FROM pragma_table_info('ship_snapshots') WHERE name = 'total_threads')", [], |r| r.get(0))
            .unwrap();
        assert!(has, "the column was added");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_v10_ledger_numbers_its_convoys_in_the_order_they_opened() {
        let dir = std::env::temp_dir().join(format!("ledger-migrate-v11-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("ledger.db").to_string_lossy().into_owned();
        drop(Ledger::open(&path).unwrap());
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "DROP INDEX convoy_no; ALTER TABLE convoys DROP COLUMN no; ALTER TABLE convoys DROP COLUMN goal; ALTER TABLE convoys DROP COLUMN standing;
             INSERT INTO convoys(key, created_at) VALUES ('PROJ-9', 't2'), ('PROJ-5', 't1'), ('PROJ-7', 't3');
             PRAGMA user_version = 10;",
        )
        .unwrap();
        drop(conn);
        let db = Ledger::open(&path).unwrap();
        let numbered: Vec<_> = (1..=3).map(|n| db.convoy_by_no(n).unwrap().unwrap().0).collect();
        assert_eq!(numbered, ["PROJ-5", "PROJ-9", "PROJ-7"], "by the order they opened");
        db.open(&ConvoyKey("PROJ-1".into()), None, false, "t4").unwrap();
        assert_eq!(db.convoy_by_no(4).unwrap(), Some(ConvoyKey("PROJ-1".into())));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_v7_ledger_seeds_each_standing_plan_as_its_history() {
        let dir = std::env::temp_dir().join(format!("ledger-migrate-v8-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("ledger.db").to_string_lossy().into_owned();
        drop(Ledger::open(&path).unwrap());
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "INSERT INTO convoys(key, created_at) VALUES ('PROJ-100', 't1');
             INSERT INTO convoy_plans(convoy, body, updated_at) VALUES ('PROJ-100', 'ship the seed', 't2');
             ALTER TABLE inspections DROP COLUMN case_id; ALTER TABLE ship_snapshots DROP COLUMN threads_changed_at; DROP INDEX convoy_no; ALTER TABLE convoys DROP COLUMN no; ALTER TABLE convoys DROP COLUMN goal; ALTER TABLE convoys DROP COLUMN standing; PRAGMA user_version = 7;",
        )
        .unwrap();
        drop(conn);

        let db = Ledger::open(&path).unwrap();
        let history = db.plan_history(&ConvoyKey("PROJ-100".into())).unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!((history[0].created_at.as_str(), &history[0].payload["plan"]), ("t2", &serde_json::json!("ship the seed")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_v4_ledger_rebuilds_sessions_to_take_an_admiral_and_keeps_its_rows() {
        let dir = std::env::temp_dir().join(format!("ledger-migrate-v5-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("ledger.db").to_string_lossy().into_owned();
        let db = Ledger::open(&path).unwrap();
        TicketRepo::upsert(&db, &bare_ticket("PROJ-1")).unwrap();
        let worker = db.start(&worker_on("PROJ-1")).unwrap().expect("a session");
        let state = CogState::Worker(WorkerState::Escalated);
        db.record(&Report { session: worker.clone(), state, last_action: None, reported_at: "t1".into() }).unwrap();
        drop(db);
        // Back to v4: the same rebuild, without the admiral kind.
        let v4 = MIGRATIONS[3].replace(",'admiral'", ""); // v5's rebuild
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(&format!("PRAGMA foreign_keys = OFF; {v4} ALTER TABLE ships DROP COLUMN rounds; ALTER TABLE inspections DROP COLUMN case_id; ALTER TABLE ship_snapshots DROP COLUMN threads_changed_at; DROP INDEX convoy_no; ALTER TABLE convoys DROP COLUMN no; ALTER TABLE convoys DROP COLUMN goal; ALTER TABLE convoys DROP COLUMN standing; PRAGMA user_version = 4;"))
            .unwrap();
        drop(conn);

        let db = Ledger::open(&path).unwrap();
        assert_eq!(schema_version(&path).unwrap(), SCHEMA_VERSION, "stamped current");
        assert_eq!(ReportRepo::latest(&db, &worker).unwrap().map(|r| r.state), Some(state), "rows kept");
        let admiral = Session { kind: CrewKind::Admiral, tmux_session: "admiral-PROJ-1".into(), ..worker_on("PROJ-1") };
        assert!(db.start(&admiral).unwrap().is_some(), "an admiral is a kind now");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_snapshot_is_a_standalone_copy_of_the_live_ledger() {
        let dir = std::env::temp_dir().join(format!("ledger-snapshot-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (live, copy) = (dir.join("ledger.db"), dir.join("copy.db"));
        let (live, copy) = (live.to_string_lossy().into_owned(), copy.to_string_lossy().into_owned());
        let db = Ledger::open(&live).unwrap(); // stays open, as fleetd's writer would
        db.setting_set("poll_secs", "9").unwrap();
        snapshot(&live, &copy).unwrap();
        assert!(!std::path::Path::new(&format!("{copy}-wal")).exists(), "no WAL beside the copy");
        assert_eq!(schema_version(&copy).unwrap(), SCHEMA_VERSION);
        let restored = Ledger::open_readonly(&copy).unwrap();
        assert_eq!(restored.setting_get("poll_secs").unwrap().as_deref(), Some("9"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    pub(super) fn bare_ticket(key: &str) -> Ticket {
        Ticket {
            key: TicketKey(key.into()),
            convoy: None,
            held: false,
            created_at: "2026-09-30T00:00:00Z".into(),
            blocked_by: vec![],
            title: None,
            assignee: None,
            status: None,
            synced_at: None,
        }
    }

    pub(super) fn worker_on(key: &str) -> Session {
        Session {
            id: SessionId(0), // ignored; the DB assigns
            kind: CrewKind::Worker,
            ticket: Some(TicketKey(key.into())),
            convoy: None,
            worktree_path: "/wt".into(),
            tmux_session: format!("worker-{key}"),
            harness_session_id: None,
            created_at: "t".into(),
            ended_at: None,
            anchored_at: None,
            ship: None,
        }
    }

    fn captain_on(convoy: &str) -> Session {
        Session {
            id: SessionId(0),
            kind: CrewKind::Captain,
            ticket: None, // a captain carries no ticket...
            convoy: Some(ConvoyKey(convoy.into())), // ...it commands a convoy
            worktree_path: "/wt".into(),
            tmux_session: format!("captain-{convoy}"),
            harness_session_id: None,
            created_at: "t".into(),
            ended_at: None,
            anchored_at: None,
            ship: None,
        }
    }

    #[test]
    fn the_memory_sets_by_id_keeps_its_order_and_its_history() {
        let db = Ledger::in_memory().unwrap();
        let refused = |id: &str, why: &str| Refused { id: id.into(), what: "a batch create".into(), why: why.into(), ruling: "operator".into() };
        let ruling = StandingRuling { id: "rev-race".into(), when: "master took the revs".into(), then: "re-stamp".into(), why: "bookkeeping".into(), ruling: "operator".into() };
        let first = Memory { refused: vec![refused("batch", "one at a time"), refused("tenant", "operators live home")], rulings: vec![ruling.clone()] };
        assert_eq!(db.remember(&first, "t1").unwrap(), 3);
        assert_eq!(db.memory().unwrap(), first);

        let edit = Memory { refused: vec![refused("batch", "each governed alone")], rulings: vec![ruling.clone()] };
        assert_eq!(db.remember(&edit, "t2").unwrap(), 1, "only the changed entry; the rest stand");
        let now = db.memory().unwrap();
        assert_eq!(now.refused.iter().map(|r| (r.id.as_str(), r.why.as_str())).collect::<Vec<_>>(), [("batch", "each governed alone"), ("tenant", "operators live home")], "updated in its place");

        db.forget("tenant", "t3").unwrap();
        assert!(db.forget("tenant", "t4").is_err(), "already gone");
        assert_eq!(db.memory().unwrap().refused.len(), 1);
        let versions: i64 = db.conn.query_row("SELECT COUNT(*) FROM memory", [], |r| r.get(0)).unwrap();
        assert_eq!(versions, 4, "every version kept");

        let bad = Memory { refused: vec![], rulings: vec![StandingRuling { then: " ".into(), ..ruling }] };
        assert!(db.remember(&bad, "t5").is_err(), "the schema refuses an empty then");
        assert_eq!(db.memory().unwrap().rulings[0].then, "re-stamp", "and keeps the memory as it was");
    }

    #[test]
    fn ticket_round_trips_with_edges_and_cache() {
        let db = Ledger::in_memory().unwrap();
        let mut t = bare_ticket("PROJ-1");
        t.title = Some("Do X".into());
        t.assignee = Some("me".into());
        t.status = Some(TicketStatus::InProgress);
        t.synced_at = Some("t".into());
        TicketRepo::upsert(&db, &t).unwrap();
        TicketRepo::upsert(&db, &bare_ticket("PROJ-2")).unwrap(); // FK target for the edge

        db.set_blocked_by(&TicketKey("PROJ-1".into()), &[TicketKey("PROJ-2".into())])
            .unwrap();

        let got = TicketRepo::get(&db, &TicketKey("PROJ-1".into())).unwrap().unwrap();
        assert_eq!(got.title.as_deref(), Some("Do X"));
        assert_eq!(got.status, Some(TicketStatus::InProgress));
        assert_eq!(got.blocked_by, vec![TicketKey("PROJ-2".into())]);
        assert_eq!(db.board().unwrap().len(), 2);
    }

    #[test]
    fn upsert_updates_the_cache_in_place() {
        let db = Ledger::in_memory().unwrap();
        TicketRepo::upsert(&db, &bare_ticket("PROJ-1")).unwrap();
        let mut t = bare_ticket("PROJ-1");
        t.status = Some(TicketStatus::Done);
        TicketRepo::upsert(&db, &t).unwrap(); // second upsert = update, not a dup
        assert_eq!(db.board().unwrap().len(), 1);
        assert_eq!(
            TicketRepo::get(&db, &TicketKey("PROJ-1".into())).unwrap().unwrap().status,
            Some(TicketStatus::Done)
        );
    }

    #[test]
    fn session_start_live_end_roundtrip() {
        let db = Ledger::in_memory().unwrap();
        TicketRepo::upsert(&db, &bare_ticket("PROJ-1")).unwrap();
        let id = db.start(&worker_on("PROJ-1")).unwrap().expect("a new session");
        assert_eq!(db.live().unwrap().len(), 1);
        assert_eq!(SessionRepo::get(&db, &id).unwrap().unwrap().kind, CrewKind::Worker);
        db.end(&id, "t2").unwrap();
        assert!(db.live().unwrap().is_empty());
        assert_eq!(SessionRepo::get(&db, &id).unwrap().unwrap().ended_at.as_deref(), Some("t2"));
    }

    #[test]
    fn spawn_lock_skips_a_second_live_worker() {
        let db = Ledger::in_memory().unwrap();
        TicketRepo::upsert(&db, &bare_ticket("PROJ-1")).unwrap();
        let id = db.start(&worker_on("PROJ-1")).unwrap().expect("a new session");
        assert!(
            db.start(&worker_on("PROJ-1")).unwrap().is_none(),
            "a 2nd live worker is skipped by the lock (None), not an error"
        );
        db.end(&id, "t2").unwrap();
        assert!(
            db.start(&worker_on("PROJ-1")).unwrap().is_some(),
            "respawn ok once the first ended"
        );
    }

    #[test]
    fn fk_rejects_a_session_for_an_unknown_ticket() {
        let db = Ledger::in_memory().unwrap();
        assert!(db.start(&worker_on("NOPE")).is_err(), "FK should reject a dangling ticket ref");
    }

    #[test]
    fn captain_starts_ticketless_on_a_convoy() {
        // No ticket row needed: a captain has no ticket (and convoy carries no FK).
        let db = Ledger::in_memory().unwrap();
        let id = db.start(&captain_on("PROJ-100")).unwrap().expect("a captain session");
        let got = SessionRepo::get(&db, &id).unwrap().unwrap();
        assert_eq!(got.kind, CrewKind::Captain);
        assert!(got.ticket.is_none(), "a captain carries no ticket");
        assert_eq!(got.convoy, Some(ConvoyKey("PROJ-100".into())));
    }

    #[test]
    fn a_convoys_last_captain_launch_and_finish_are_its_newest() {
        let db = Ledger::in_memory().unwrap();
        let convoy = ConvoyKey("PROJ-100".into());
        assert_eq!(db.last_captain_launch(&convoy).unwrap(), None, "never launched");
        let first = db.start(&Session { created_at: "2026-01-01T00:00:00Z".into(), ..captain_on("PROJ-100") }).unwrap().unwrap();
        SessionRepo::end(&db, &first, "2026-01-01T00:05:00Z").unwrap();
        db.start(&Session { created_at: "2026-01-02T00:00:00Z".into(), ..captain_on("PROJ-100") }).unwrap().unwrap();
        assert_eq!(db.last_captain_launch(&convoy).unwrap().as_deref(), Some("2026-01-02T00:00:00Z"));
        assert_eq!(db.last_captain_finish(&convoy).unwrap().as_deref(), Some("2026-01-01T00:05:00Z"), "the live one hasn't finished");
    }

    #[test]
    fn syncing_a_product_owner_ticket_marks_it_for_a_hand() {
        let db = Ledger::in_memory().unwrap();
        let t = |key: &str, labels: Vec<String>| TrackerTicket {
            key: TicketKey(key.into()),
            title: "T".into(),
            status: TicketStatus::Todo,
            assignee: None,
            convoy: None,
            blocked_by: vec![],
            labels,
        };
        db.sync(&t("PROJ-1", vec![TicketLabel::ProductOwner.as_str().into()]), "t").unwrap();
        db.sync(&t("PROJ-2", vec!["other".into()]), "t").unwrap();
        assert!(db.is_hand(&TicketKey("PROJ-1".into())).unwrap());
        assert!(!db.is_hand(&TicketKey("PROJ-2".into())).unwrap());
    }

    #[test]
    fn check_rejects_a_ticketless_worker() {
        let db = Ledger::in_memory().unwrap();
        let mut s = worker_on("PROJ-1");
        s.ticket = None; // the per-kind CHECK fires before any FK check
        assert!(db.start(&s).is_err(), "a worker with no ticket violates the per-kind CHECK");
    }

    #[test]
    fn check_rejects_a_convoyless_captain() {
        let db = Ledger::in_memory().unwrap();
        let mut s = captain_on("PROJ-100");
        s.convoy = None;
        assert!(db.start(&s).is_err(), "a captain with no convoy violates the per-kind CHECK");
    }

    #[test]
    fn one_live_captain_per_convoy() {
        let db = Ledger::in_memory().unwrap();
        let id = db.start(&captain_on("PROJ-100")).unwrap().expect("first captain");
        assert!(
            db.start(&captain_on("PROJ-100")).unwrap().is_none(),
            "a 2nd live captain on the same convoy is skipped by the lock (None)"
        );
        assert!(
            db.start(&captain_on("PROJ-200")).unwrap().is_some(),
            "a different convoy is an independent slot"
        );
        db.end(&id, "t2").unwrap();
        assert!(
            db.start(&captain_on("PROJ-100")).unwrap().is_some(),
            "respawn ok once the first captain ended"
        );
    }

    #[test]
    fn convoy_open_is_active_and_close_deactivates() {
        let db = Ledger::in_memory().unwrap();
        db.open(&ConvoyKey("PROJ-100".into()), Some("auth"), false, "t1").unwrap();
        let active = db.active().unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].key, ConvoyKey("PROJ-100".into()));
        assert_eq!(active[0].title.as_deref(), Some("auth"));

        db.close(&ConvoyKey("PROJ-100".into()), "t2").unwrap();
        assert!(db.active().unwrap().is_empty(), "a closed convoy drops out of active");
    }

    #[test]
    fn a_ticket_is_in_one_convoy_the_one_it_was_added_to_else_its_epics() {
        let db = Ledger::in_memory().unwrap();
        let epic_child = |key: &str, epic: &str| TrackerTicket {
            key: TicketKey(key.into()),
            title: "T".into(),
            status: TicketStatus::Todo,
            assignee: None,
            convoy: Some(ConvoyKey(epic.into())),
            blocked_by: vec![],
            labels: vec![],
        };
        db.sync(&epic_child("PROJ-1", "PROJ-50"), "t").unwrap();
        let (ours, other) = (ConvoyKey("PROJ-90".into()), ConvoyKey("PROJ-91".into()));
        let k = TicketKey("PROJ-1".into());
        let convoy_of = |db: &Ledger| TicketRepo::get(db, &k).unwrap().unwrap().convoy;

        assert_eq!(db.add_member(&ConvoyKey("PROJ-50".into()), &k, "t").unwrap(), None, "its epic's: already a member");
        assert_eq!(db.add_member(&ours, &k, "t").unwrap(), Some(ConvoyKey("PROJ-50".into())), "left its epic's");
        assert_eq!(convoy_of(&db), Some(ours.clone()));
        assert_eq!(db.board().unwrap()[0].convoy, Some(ours.clone()), "the board reads it the same way");
        assert_eq!(db.add_member(&ours, &k, "t").unwrap(), None, "idempotent");
        assert!(db.add_member(&other, &k, "t").is_err(), "never in two convoys");

        db.sync(&epic_child("PROJ-1", "PROJ-51"), "t2").unwrap(); // re-parented in the tracker
        assert_eq!(convoy_of(&db), Some(ours.clone()), "the fleet's membership survives a sync");
        assert_eq!(db.added_members().unwrap(), vec![k.clone()]);

        assert!(db.remove_member(&other, &k).is_err(), "wasn't added there");
        db.remove_member(&ours, &k).unwrap();
        assert_eq!(convoy_of(&db), Some(ConvoyKey("PROJ-51".into())), "back to its epic's");
        assert!(db.add_member(&ours, &TicketKey("PROJ-404".into()), "t").is_err(), "not on the board");
    }

    #[test]
    fn a_convoy_plan_is_set_replaced_and_cleared_and_never_blank() {
        let db = Ledger::in_memory().unwrap();
        let k = ConvoyKey("PROJ-100".into());
        db.open(&k, None, false, "t1").unwrap();
        assert_eq!(db.plan(&k).unwrap(), None, "no plan until one is given");
        db.set_plan(&k, Some("ship the seed"), "t2").unwrap();
        db.set_plan(&k, Some("ship the seed, then the quorums"), "t3").unwrap();
        assert_eq!(db.plan(&k).unwrap().as_deref(), Some("ship the seed, then the quorums"));
        assert!(db.set_plan(&k, Some("  \n"), "t4").is_err(), "a blank plan is refused");
        db.set_plan(&k, None, "t5").unwrap();
        assert_eq!(db.plan(&k).unwrap(), None);
    }

    #[test]
    fn a_convoy_plan_keeps_its_history_and_an_unchanged_one_logs_nothing() {
        let db = Ledger::in_memory().unwrap();
        let k = ConvoyKey("PROJ-100".into());
        db.open(&k, None, false, "t1").unwrap();
        db.set_plan(&k, Some("ship the seed"), "t2").unwrap();
        db.set_plan(&k, Some("ship the seed"), "t3").unwrap(); // a re-open with the same plan
        db.set_plan(&k, Some("ship the seed, then the quorums"), "t4").unwrap();
        db.set_plan(&k, None, "t5").unwrap();
        db.set_plan(&k, None, "t6").unwrap();
        let _ = db.set_plan(&k, Some(" "), "t7"); // refused: no event either
        let history: Vec<_> = db.plan_history(&k).unwrap().into_iter().map(|e| (e.created_at, e.payload["plan"].clone())).collect();
        assert_eq!(
            history,
            [
                ("t2".to_string(), serde_json::json!("ship the seed")),
                ("t4".to_string(), serde_json::json!("ship the seed, then the quorums")),
                ("t5".to_string(), serde_json::Value::Null),
            ]
        );
    }

    #[test]
    fn a_convoy_goal_is_one_of_its_live_tickets_and_stays_until_another_is_set() {
        let db = Ledger::in_memory().unwrap();
        let ticket = |key: &str, epic: &str, status: TicketStatus| TrackerTicket {
            key: TicketKey(key.into()),
            title: "T".into(),
            status,
            assignee: None,
            convoy: Some(ConvoyKey(epic.into())),
            blocked_by: vec![],
            labels: vec![],
        };
        for (key, epic, status) in [("PROJ-1", "PROJ-50", TicketStatus::Todo), ("PROJ-2", "PROJ-50", TicketStatus::Cancelled), ("PROJ-3", "PROJ-60", TicketStatus::Todo), ("PROJ-4", "PROJ-60", TicketStatus::Todo)] {
            db.sync(&ticket(key, epic, status), "t").unwrap();
        }
        let (k, goal) = (ConvoyKey("PROJ-50".into()), TicketKey("PROJ-1".into()));
        db.open(&k, None, false, "t1").unwrap();
        assert_eq!(db.goal(&k).unwrap(), None, "no goal until one is set");
        assert!(db.set_goal(&k, &TicketKey("PROJ-3".into()), "t2").is_err(), "another convoy's ticket");
        assert!(db.set_goal(&k, &TicketKey("PROJ-2".into()), "t2").is_err(), "a Cancelled ticket proves nothing");
        assert!(db.set_goal(&k, &TicketKey("PROJ-404".into()), "t2").is_err(), "not on the board");
        db.set_goal(&k, &goal, "t2").unwrap();
        db.set_goal(&k, &goal, "t3").unwrap(); // unchanged: no event
        assert_eq!(db.goal(&k).unwrap(), Some(goal.clone()));
        assert_eq!(db.active().unwrap()[0].goal, Some(goal.clone()), "the convoy row carries it");
        let set: Vec<_> = db.events_of(EventKind::ConvoyGoalSet).unwrap().into_iter().map(|e| e.created_at).collect();
        assert_eq!(set, ["t2"]);

        let theirs = ConvoyKey("PROJ-60".into());
        let added = TicketKey("PROJ-4".into());
        db.add_member(&k, &added, "t4").unwrap();
        db.set_goal(&k, &added, "t5").unwrap();
        assert!(db.remove_member(&k, &added).is_err(), "the goal stays in its convoy");
        db.set_goal(&k, &goal, "t6").unwrap();
        db.remove_member(&k, &added).unwrap();
        assert_eq!(TicketRepo::get(&db, &added).unwrap().unwrap().convoy, Some(theirs));
    }

    #[test]
    fn convoy_reopen_clears_ended_at() {
        let db = Ledger::in_memory().unwrap();
        let k = ConvoyKey("PROJ-100".into());
        db.open(&k, None, false, "t1").unwrap();
        db.close(&k, "t2").unwrap();
        db.open(&k, Some("revived"), false, "t3").unwrap();
        let active = db.active().unwrap();
        assert_eq!(active.len(), 1, "re-opening clears ended_at");
        assert_eq!(active[0].title.as_deref(), Some("revived"));
    }

    #[test]
    fn convoy_open_and_close_log_cdc_events_and_a_noop_close_logs_nothing() {
        let db = Ledger::in_memory().unwrap();
        let k = ConvoyKey("PROJ-100".into());
        db.open(&k, None, false, "t1").unwrap();
        db.close(&k, "t2").unwrap();
        db.close(&k, "t3").unwrap(); // already closed → no-op
        let kinds: Vec<_> = db.since(Seq(0)).unwrap().into_iter().map(|e| e.kind).collect();
        assert!(kinds.contains(&EventKind::ConvoyOpened));
        assert_eq!(
            kinds.iter().filter(|k| **k == EventKind::ConvoyClosed).count(),
            1,
            "a no-op close logs nothing"
        );
    }

    #[test]
    fn anchor_sets_the_flag_weigh_clears_it_and_both_log_cdc_once() {
        let db = Ledger::in_memory().unwrap();
        TicketRepo::upsert(&db, &bare_ticket("PROJ-1")).unwrap();
        let id = db.start(&worker_on("PROJ-1")).unwrap().expect("a session");

        SessionRepo::anchor(&db, &id, "t1").unwrap();
        assert!(SessionRepo::get(&db, &id).unwrap().unwrap().anchored_at.is_some());
        SessionRepo::anchor(&db, &id, "t1b").unwrap(); // already anchored → no-op

        SessionRepo::weigh(&db, &id, "t2").unwrap();
        assert!(SessionRepo::get(&db, &id).unwrap().unwrap().anchored_at.is_none());
        SessionRepo::weigh(&db, &id, "t2b").unwrap(); // not anchored → no-op

        let kinds: Vec<_> = db.since(Seq(0)).unwrap().into_iter().map(|e| e.kind).collect();
        assert_eq!(
            kinds.iter().filter(|k| **k == EventKind::SessionAnchored).count(),
            1,
            "a no-op anchor logs nothing"
        );
        assert_eq!(
            kinds.iter().filter(|k| **k == EventKind::SessionWeighed).count(),
            1,
            "a no-op weigh logs nothing"
        );
    }

    #[test]
    fn set_held_flips_the_flag_logs_cdc_once_each_way_and_carries_the_reason() {
        let db = Ledger::in_memory().unwrap();
        TicketRepo::upsert(&db, &bare_ticket("PROJ-1")).unwrap();
        let key = TicketKey("PROJ-1".into());

        TicketRepo::set_held(&db, &key, true, Some("needs grooming"), "t1").unwrap();
        assert!(TicketRepo::get(&db, &key).unwrap().unwrap().held);
        TicketRepo::set_held(&db, &key, true, Some("again"), "t1b").unwrap(); // already held → no-op

        TicketRepo::set_held(&db, &key, false, None, "t2").unwrap();
        assert!(!TicketRepo::get(&db, &key).unwrap().unwrap().held);
        TicketRepo::set_held(&db, &key, false, None, "t2b").unwrap(); // not held → no-op

        let events = db.since(Seq(0)).unwrap();
        let held: Vec<_> = events.iter().filter(|e| e.kind == EventKind::TicketHeld).collect();
        let released = events.iter().filter(|e| e.kind == EventKind::TicketReleased).count();
        assert_eq!(held.len(), 1, "a no-op hold logs nothing");
        assert_eq!(released, 1, "a no-op release logs nothing");
        assert_eq!(held[0].payload["reason"], "needs grooming", "the reason rides on the event");
        assert_eq!(db.held_reason(&key).unwrap().as_deref(), Some("needs grooming"), "and reads back");
    }

    #[test]
    fn report_keeps_latest_and_logs_history() {
        let db = Ledger::in_memory().unwrap();
        TicketRepo::upsert(&db, &bare_ticket("PROJ-1")).unwrap();
        let id = db.start(&worker_on("PROJ-1")).unwrap().expect("a new session");

        db.record(&Report {
            session: id.clone(),
            state: CogState::Worker(WorkerState::Implementing),
            last_action: Some("wrote the parser".into()),
            reported_at: "t1".into(),
        })
        .unwrap();
        // A second report REPLACES the latest row (one latest per session)...
        db.record(&Report {
            session: id.clone(),
            state: CogState::Worker(WorkerState::Concluded),
            last_action: None,
            reported_at: "t2".into(),
        })
        .unwrap();

        let got = ReportRepo::latest(&db, &id).unwrap().unwrap();
        assert_eq!(got.state, CogState::Worker(WorkerState::Concluded));
        assert_eq!(got.reported_at, "t2");

        // ...but BOTH land in the event log as history.
        let reported: Vec<_> = db
            .since(Seq(0))
            .unwrap()
            .into_iter()
            .filter(|e| e.kind == EventKind::StatusReported)
            .collect();
        assert_eq!(reported.len(), 2);
        assert_eq!(reported[0].entity_kind, EntityKind::Session);
        assert_eq!(reported[1].payload["state"], "concluded");
    }
}

// ── ships (+ snapshots) ─────────────────────────────────────────────────────────

fn row_to_ship(r: &rusqlite::Row) -> rusqlite::Result<Ship> {
    Ok(Ship {
        id: ShipId(r.get("id")?),
        ticket: r.get::<_, Option<String>>("ticket")?.map(ticket_key).transpose()?,
        iid: r.get("iid")?,
        url: r.get("url")?,
        target: r.get("target")?,
        source: r.get("source")?,
        pilot: r.get("pilot")?,
        created_at: r.get("created_at")?,
        ended_at: r.get("ended_at")?,
        swept: r.get("swept")?,
        reviewed: r.get("reviewed")?,
        rounds: r.get("rounds")?,
    })
}

fn row_to_snapshot(r: &rusqlite::Row) -> rusqlite::Result<ShipSnapshot> {
    let state: String = r.get("state")?;
    let pipeline: Option<String> = r.get("pipeline")?;
    let pipeline = match pipeline {
        Some(p) => Some(require(PipelineStatus::parse(&p), 11, "pipeline")?),
        None => None,
    };
    Ok(ShipSnapshot {
        ship: ShipId(r.get("ship_id")?),
        title: r.get("title")?,
        state: require(ShipState::parse(&state), 2, "ship state")?,
        head_sha: r.get("head_sha")?,
        has_conflicts: r.get::<_, i64>("has_conflicts")? != 0,
        open_threads: r.get("open_threads")?,
        total_threads: r.get("total_threads")?,
        approvals: Approvals {
            current: r.get("approvals_current")?,
            required: r.get("approvals_required")?,
        },
        armed: r.get::<_, i64>("armed")? != 0,
        on_merge_train: r.get::<_, i64>("on_merge_train")? != 0,
        pipeline,
        blocked_by: {
            let json: String = r.get("blocked_by_json")?;
            serde_json::from_str(&json).map_err(|e| rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e)))?
        },
        merge_status: r.get("merge_status")?,
        synced_at: r.get("synced_at")?,
    })
}

impl ShipRepo for Ledger {
    fn upsert(&self, s: &Ship) -> Result<ShipId> {
        let id: i64 = self.conn.query_row(
            "INSERT INTO ships(ticket, iid, url, target, source, pilot, created_at, ended_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(iid) DO UPDATE SET
               ticket   = excluded.ticket,
               url      = excluded.url,
               target   = excluded.target,
               source   = excluded.source,
               pilot    = excluded.pilot,
               ended_at = excluded.ended_at
             RETURNING id",
            params![
                s.ticket.as_ref().map(|k| &k.0),
                s.iid,
                s.url,
                s.target,
                s.source,
                s.pilot,
                s.created_at,
                s.ended_at,
            ],
            |r| r.get(0),
        )?;
        Ok(ShipId(id))
    }

    fn retire(&self, id: &ShipId, at: &str) -> Result<()> {
        self.conn.execute("UPDATE ships SET ended_at = ?2 WHERE id = ?1 AND ended_at IS NULL", params![id.0, at])?;
        Ok(())
    }

    fn get_by_iid(&self, iid: i64) -> Result<Option<Ship>> {
        Ok(self
            .conn
            .query_row("SELECT * FROM ships WHERE iid = ?1", params![iid], row_to_ship)
            .optional()?)
    }

    fn armada(&self) -> Result<Vec<Ship>> {
        let mut stmt = self
            .conn
            .prepare("SELECT * FROM ships WHERE ended_at IS NULL ORDER BY iid")?;
        let rows = stmt.query_map([], row_to_ship)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    fn sync_snapshot(&self, s: &ShipSnapshot) -> Result<()> {
        self.conn.execute(
            "INSERT INTO ship_snapshots(ship_id, title, state, head_sha, has_conflicts, open_threads, total_threads,
                 approvals_current, approvals_required, armed, on_merge_train, pipeline, merge_status, synced_at,
                 blocked_by_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
             ON CONFLICT(ship_id) DO UPDATE SET
               threads_changed_at = CASE
                 WHEN open_threads <> excluded.open_threads OR total_threads <> excluded.total_threads THEN excluded.synced_at
                 ELSE threads_changed_at END,
               title=excluded.title, state=excluded.state, head_sha=excluded.head_sha,
               has_conflicts=excluded.has_conflicts, open_threads=excluded.open_threads, total_threads=excluded.total_threads,
               approvals_current=excluded.approvals_current, approvals_required=excluded.approvals_required,
               armed=excluded.armed, on_merge_train=excluded.on_merge_train, pipeline=excluded.pipeline,
               merge_status=excluded.merge_status, synced_at=excluded.synced_at, blocked_by_json=excluded.blocked_by_json",
            params![
                s.ship.0, s.title, s.state.as_str(), s.head_sha, s.has_conflicts as i64,
                s.open_threads, s.total_threads, s.approvals.current, s.approvals.required, s.armed as i64,
                s.on_merge_train as i64, s.pipeline.map(|p| p.as_str()), s.merge_status, s.synced_at,
                serde_json::to_string(&s.blocked_by)?,
            ],
        )?;
        Ok(())
    }

    fn snapshot(&self, id: &ShipId) -> Result<Option<ShipSnapshot>> {
        Ok(self
            .conn
            .query_row("SELECT * FROM ship_snapshots WHERE ship_id = ?1", params![id.0], row_to_snapshot)
            .optional()?)
    }

    fn threads_changed_at(&self, id: &ShipId) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row("SELECT threads_changed_at FROM ship_snapshots WHERE ship_id = ?1", params![id.0], |r| r.get(0))
            .optional()?
            .flatten())
    }

    fn set_reviewed(&self, id: &ShipId, head_sha: Option<&str>) -> Result<()> {
        self.conn
            .execute("UPDATE ships SET reviewed = ?2 WHERE id = ?1", params![id.0, head_sha])?;
        Ok(())
    }

    fn count_round(&self, id: &ShipId) -> Result<()> {
        self.conn.execute("UPDATE ships SET rounds = rounds + 1 WHERE id = ?1", params![id.0])?;
        Ok(())
    }


    fn set_swept(&self, id: &ShipId, fingerprint: Option<&str>) -> Result<()> {
        self.conn
            .execute("UPDATE ships SET swept = ?2 WHERE id = ?1", params![id.0, fingerprint])?;
        Ok(())
    }
}

// ── orders (the dispatch queue) ─────────────────────────────────────────────────

fn row_to_order(r: &rusqlite::Row) -> rusqlite::Result<Order> {
    Ok(Order {
        ticket: ticket_key(r.get("ticket")?)?,
        auto_impl: r.get::<_, i64>("auto_impl")? != 0,
        fork_from: r.get("fork_from")?,
        ac_summary: r.get("ac_summary")?,
        park_at: r.get("park_at")?,
        worktree_path: r.get("worktree_path")?,
        created_at: r.get("created_at")?,
    })
}

impl OrderRepo for Ledger {
    fn dispatch(&self, o: &Order) -> Result<()> {
        // Upsert the order + append a Dispatched event in one txn (CDC — the order
        // and the change-log never diverge; the follow-stream sees the new order).
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO orders(ticket, auto_impl, fork_from, ac_summary, park_at, worktree_path, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(ticket) DO UPDATE SET
               auto_impl=excluded.auto_impl,
               fork_from=excluded.fork_from, ac_summary=excluded.ac_summary, park_at=excluded.park_at,
               worktree_path=excluded.worktree_path",
            params![
                o.ticket.0, o.auto_impl as i64,
                o.fork_from, o.ac_summary, o.park_at, o.worktree_path, o.created_at,
            ],
        )?;
        let payload =
            serde_json::json!({ "auto_impl": o.auto_impl, "park_at": o.park_at });
        tx.execute(
            "INSERT INTO events(created_at, kind, entity_kind, entity_ref, payload_json)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                o.created_at,
                EventKind::Dispatched.as_str(),
                EntityKind::Ticket.as_str(),
                o.ticket.0,
                serde_json::to_string(&payload)?,
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    fn rework(&self, key: &TicketKey, auto_impl: bool, at: &str) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        let n = tx.execute(
            "UPDATE orders SET auto_impl = ?2, created_at = ?3 WHERE ticket = ?1",
            params![key.0, auto_impl as i64, at],
        )?;
        anyhow::ensure!(n == 1, "{} has no order to rework (dispatch it)", key.0);
        tx.execute(
            "INSERT INTO events(created_at, kind, entity_kind, entity_ref, payload_json)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                at,
                EventKind::Dispatched.as_str(),
                EntityKind::Ticket.as_str(),
                key.0,
                serde_json::json!({ "auto_impl": auto_impl, "rework": true }).to_string(),
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    fn get(&self, key: &TicketKey) -> Result<Option<Order>> {
        Ok(self
            .conn
            .query_row("SELECT * FROM orders WHERE ticket = ?1", params![key.0], row_to_order)
            .optional()?)
    }

    fn queue(&self) -> Result<Vec<Order>> {
        let mut stmt = self.conn.prepare("SELECT * FROM orders ORDER BY created_at")?;
        let rows = stmt.query_map([], row_to_order)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    fn cancel(&self, key: &TicketKey) -> Result<()> {
        self.conn.execute("DELETE FROM orders WHERE ticket = ?1", params![key.0])?;
        Ok(())
    }
}

// ── convoys ─────────────────────────────────────────────────────────────────────

fn row_to_convoy(r: &rusqlite::Row) -> rusqlite::Result<Convoy> {
    Ok(Convoy {
        key: convoy_key(r.get("key")?)?,
        // Read-only readers (the dashboard, fleetc) can run ahead of fleetd's migration: a ledger
        // without the column yet reads as unnumbered (0).
        no: match r.as_ref().column_index("no") {
            Ok(i) => r.get::<_, Option<i64>>(i)?.unwrap_or_default(),
            Err(_) => 0,
        },
        title: r.get("title")?,
        created_at: r.get("created_at")?,
        ended_at: r.get("ended_at")?,
        anchored_at: r.get("anchored_at")?,
        goal: match r.as_ref().column_index("goal") {
            Ok(i) => r.get::<_, Option<String>>(i)?.map(ticket_key).transpose()?,
            Err(_) => None,
        },
        standing: match r.as_ref().column_index("standing") {
            Ok(i) => r.get(i)?,
            Err(_) => false,
        },
    })
}

impl ConvoyRepo for Ledger {
    fn add_member(&self, convoy: &ConvoyKey, ticket: &TicketKey, at: &str) -> Result<Option<ConvoyKey>> {
        let added: Option<String> = self
            .conn
            .query_row("SELECT convoy FROM convoy_members WHERE ticket = ?1", params![ticket.0], |r| r.get(0))
            .optional()?;
        match added {
            Some(c) if c == convoy.0 => return Ok(None),
            Some(c) => anyhow::bail!("{} is already in convoy {c} — `fleetc convoy remove {c} {}` first", ticket.0, ticket.0),
            None => {}
        }
        let epic: Option<String> = self
            .conn
            .query_row("SELECT convoy FROM tickets WHERE key = ?1", params![ticket.0], |r| r.get(0))
            .optional()?
            .ok_or_else(|| anyhow::anyhow!("{} isn't on the board", ticket.0))?;
        if epic.as_deref() == Some(convoy.0.as_str()) {
            return Ok(None); // already a member: its epic is this convoy
        }
        self.conn.execute(
            "INSERT INTO convoy_members(ticket, convoy, added_at) VALUES (?1, ?2, ?3)",
            params![ticket.0, convoy.0, at],
        )?;
        Ok(epic.map(convoy_key).transpose()?)
    }

    fn remove_member(&self, convoy: &ConvoyKey, ticket: &TicketKey) -> Result<()> {
        anyhow::ensure!(
            self.goal(convoy)?.as_ref() != Some(ticket),
            "{} is convoy {}'s goal — set another first (`fleetc convoy open {} --goal <KEY-N>`)",
            ticket.0,
            convoy.0,
            convoy.0
        );
        let n = self
            .conn
            .execute("DELETE FROM convoy_members WHERE ticket = ?1 AND convoy = ?2", params![ticket.0, convoy.0])?;
        anyhow::ensure!(n == 1, "{} wasn't added to convoy {} (an epic's own tickets leave it in the tracker)", ticket.0, convoy.0);
        Ok(())
    }

    fn added_members(&self) -> Result<Vec<TicketKey>> {
        let mut stmt = self.conn.prepare("SELECT ticket FROM convoy_members ORDER BY ticket")?;
        let rows = stmt.query_map([], |r| ticket_key(r.get(0)?))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    fn open(&self, key: &ConvoyKey, title: Option<&str>, standing: bool, now: &str) -> Result<()> {
        // Idempotent create; re-opening a closed convoy clears ended_at and keeps its number, its
        // title unless a new one is given, and its standing. A new one takes the next number.
        // +ConvoyOpened (CDC).
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO convoys(key, title, created_at, ended_at, no, standing)
             VALUES (?1, ?2, ?3, NULL, (SELECT COALESCE(MAX(no), 0) + 1 FROM convoys), ?4)
             ON CONFLICT(key) DO UPDATE SET title = COALESCE(excluded.title, convoys.title), ended_at = NULL,
                standing = MAX(convoys.standing, excluded.standing)",
            params![key.0, title, now, standing],
        )?;
        tx.execute(
            "INSERT INTO events(created_at, kind, entity_kind, entity_ref, payload_json)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                now,
                EventKind::ConvoyOpened.as_str(),
                EntityKind::Convoy.as_str(),
                key.0,
                serde_json::to_string(&serde_json::json!({ "title": title, "standing": standing }))?,
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    fn close(&self, key: &ConvoyKey, at: &str) -> Result<()> {
        // Set ended_at + ConvoyClosed (CDC). Only an active convoy closes; a no-op close
        // (unknown or already-closed) logs nothing.
        let tx = self.conn.unchecked_transaction()?;
        let n = tx.execute(
            "UPDATE convoys SET ended_at = ?2 WHERE key = ?1 AND ended_at IS NULL",
            params![key.0, at],
        )?;
        if n > 0 {
            tx.execute(
                "INSERT INTO events(created_at, kind, entity_kind, entity_ref, payload_json)
                 VALUES (?1, ?2, ?3, ?4, '{}')",
                params![at, EventKind::ConvoyClosed.as_str(), EntityKind::Convoy.as_str(), key.0],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    fn anchor(&self, key: &ConvoyKey, at: &str) -> Result<()> {
        // Set anchored_at + ConvoyAnchored (CDC, same txn). Only an active, not-yet-anchored convoy
        // anchors; a no-op logs nothing.
        let tx = self.conn.unchecked_transaction()?;
        let n = tx.execute(
            "UPDATE convoys SET anchored_at = ?2 WHERE key = ?1 AND anchored_at IS NULL AND ended_at IS NULL",
            params![key.0, at],
        )?;
        if n > 0 {
            tx.execute(
                "INSERT INTO events(created_at, kind, entity_kind, entity_ref, payload_json)
                 VALUES (?1, ?2, ?3, ?4, '{}')",
                params![at, EventKind::ConvoyAnchored.as_str(), EntityKind::Convoy.as_str(), key.0],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    fn weigh(&self, key: &ConvoyKey, at: &str) -> Result<()> {
        // Clear anchored_at + ConvoyWeighed (CDC, same txn). A no-op (not anchored) logs nothing.
        let tx = self.conn.unchecked_transaction()?;
        let n = tx.execute(
            "UPDATE convoys SET anchored_at = NULL WHERE key = ?1 AND anchored_at IS NOT NULL",
            params![key.0],
        )?;
        if n > 0 {
            tx.execute(
                "INSERT INTO events(created_at, kind, entity_kind, entity_ref, payload_json)
                 VALUES (?1, ?2, ?3, ?4, '{}')",
                params![at, EventKind::ConvoyWeighed.as_str(), EntityKind::Convoy.as_str(), key.0],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    fn set_plan(&self, key: &ConvoyKey, plan: Option<&str>, at: &str) -> Result<()> {
        // Write the plan + ConvoyPlanned (CDC, same txn) — the plan's history. Unchanged logs nothing.
        if self.plan(key)?.as_deref() == plan {
            return Ok(());
        }
        let tx = self.conn.unchecked_transaction()?;
        match plan {
            Some(body) => tx.execute(
                "INSERT INTO convoy_plans(convoy, body, updated_at) VALUES (?1, ?2, ?3)
                 ON CONFLICT(convoy) DO UPDATE SET body = excluded.body, updated_at = excluded.updated_at",
                params![key.0, body, at],
            )?,
            None => tx.execute("DELETE FROM convoy_plans WHERE convoy = ?1", params![key.0])?,
        };
        tx.execute(
            "INSERT INTO events(created_at, kind, entity_kind, entity_ref, payload_json) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                at,
                EventKind::ConvoyPlanned.as_str(),
                EntityKind::Convoy.as_str(),
                key.0,
                serde_json::json!({ "plan": plan }).to_string()
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    fn plan(&self, key: &ConvoyKey) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row("SELECT body FROM convoy_plans WHERE convoy = ?1", params![key.0], |r| r.get(0))
            .optional()?)
    }

    fn set_goal(&self, key: &ConvoyKey, goal: &TicketKey, at: &str) -> Result<()> {
        // Write the goal + ConvoyGoalSet (CDC, same txn). Unchanged logs nothing.
        if self.goal(key)?.as_ref() == Some(goal) {
            return Ok(());
        }
        let t = TicketRepo::get(self, goal)?.ok_or_else(|| anyhow::anyhow!("{} isn't on the board", goal.0))?;
        anyhow::ensure!(t.convoy.as_ref() == Some(key), "{} isn't in convoy {}: a convoy's goal is one of its tickets", goal.0, key.0);
        anyhow::ensure!(t.status != Some(TicketStatus::Cancelled), "{} is Cancelled: it proves nothing", goal.0);
        let tx = self.conn.unchecked_transaction()?;
        let n = tx.execute("UPDATE convoys SET goal = ?2 WHERE key = ?1", params![key.0, goal.0])?;
        anyhow::ensure!(n == 1, "no convoy {}", key.0);
        tx.execute(
            "INSERT INTO events(created_at, kind, entity_kind, entity_ref, payload_json) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                at,
                EventKind::ConvoyGoalSet.as_str(),
                EntityKind::Convoy.as_str(),
                key.0,
                serde_json::json!({ "goal": goal.0 }).to_string()
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    fn goal(&self, key: &ConvoyKey) -> Result<Option<TicketKey>> {
        // Read-only readers can run ahead of fleetd's migration: a ledger without the column has no goal.
        match self.conn.query_row("SELECT goal FROM convoys WHERE key = ?1", params![key.0], |r| r.get::<_, Option<String>>(0)) {
            Ok(goal) => goal.map(ticket_key).transpose().map_err(Into::into),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(rusqlite::Error::SqliteFailure(_, Some(m))) if m.contains("no such column") => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    fn active(&self) -> Result<Vec<Convoy>> {
        let mut stmt = self.conn.prepare("SELECT * FROM convoys WHERE ended_at IS NULL ORDER BY no")?;
        let rows = stmt.query_map([], row_to_convoy)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

// ── lookout (the review loop's one control row) ──────────────────────────────────────────

impl LookoutRepo for Ledger {
    fn anchor(&self, at: &str) -> Result<()> {
        // Set anchored_at on the one row + LookoutAnchored (CDC, same txn). No-op logs nothing.
        let tx = self.conn.unchecked_transaction()?;
        let n = tx.execute(
            "UPDATE lookout SET anchored_at = ?1 WHERE id = 1 AND anchored_at IS NULL",
            params![at],
        )?;
        if n > 0 {
            tx.execute(
                "INSERT INTO events(created_at, kind, entity_kind, entity_ref, payload_json)
                 VALUES (?1, ?2, ?3, 'lookout', '{}')",
                params![at, EventKind::LookoutAnchored.as_str(), EntityKind::Lookout.as_str()],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    fn weigh(&self, at: &str) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        let n = tx.execute(
            "UPDATE lookout SET anchored_at = NULL WHERE id = 1 AND anchored_at IS NOT NULL",
            [],
        )?;
        if n > 0 {
            tx.execute(
                "INSERT INTO events(created_at, kind, entity_kind, entity_ref, payload_json)
                 VALUES (?1, ?2, ?3, 'lookout', '{}')",
                params![at, EventKind::LookoutWeighed.as_str(), EntityKind::Lookout.as_str()],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    fn anchored(&self) -> Result<bool> {
        let at: Option<String> =
            self.conn.query_row("SELECT anchored_at FROM lookout WHERE id = 1", [], |r| r.get(0))?;
        Ok(at.is_some())
    }
}

// ── inspections ─────────────────────────────────────────────────────────────────

impl InspectionRepo for Ledger {
    fn inspect(&self, i: &Inspection) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO inspections(session_id, case_id, reported_at, verdict, body, inspected_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![i.session.0, i.case.as_ref().map(|c| c.0), i.reported_at, i.verdict.as_str(), i.body, i.inspected_at],
        )?;
        tx.execute(
            "INSERT INTO events(created_at, kind, entity_kind, entity_ref, payload_json) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                i.inspected_at,
                EventKind::EscalationInspected.as_str(),
                EntityKind::Escalation.as_str(),
                i.session.0.to_string(),
                serde_json::json!({ "verdict": i.verdict.as_str(), "reported_at": i.reported_at, "case": i.case.as_ref().map(|c| c.0) }).to_string(),
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    fn inspection(&self, session: &SessionId) -> Result<Option<Inspection>> {
        Ok(self
            .conn
            .query_row("SELECT * FROM inspections WHERE session_id = ?1 ORDER BY id DESC LIMIT 1", params![session.0], row_to_inspection)
            .optional()?)
    }
}

fn row_to_inspection(r: &rusqlite::Row) -> rusqlite::Result<Inspection> {
    let verdict: String = r.get("verdict")?;
    Ok(Inspection {
        session: SessionId(r.get("session_id")?),
        case: r.get::<_, Option<i64>>("case_id")?.map(CaseId),
        reported_at: r.get("reported_at")?,
        verdict: require(Verdict::parse(&verdict), 3, "verdict")?,
        body: r.get("body")?,
        inspected_at: r.get("inspected_at")?,
    })
}

// ── cases ───────────────────────────────────────────────────────────────────────

const CASE_SELECT: &str = "SELECT c.*, s.kind FROM cases c LEFT JOIN sessions s ON s.id = c.session_id";

fn row_to_case(r: &rusqlite::Row) -> rusqlite::Result<Case> {
    let ruling = match r.get::<_, Option<String>>("ruling")? {
        Some(answer) => Some(Ruling { question: r.get("ruling_question")?, answer, ruled_at: r.get("ruled_at")? }),
        None => None,
    };
    Ok(Case {
        id: CaseId(r.get("id")?),
        ticket: TicketKey(r.get("ticket")?),
        convoy: r.get::<_, Option<String>>("convoy")?.map(ConvoyKey),
        session: r.get::<_, Option<i64>>("session_id")?.map(SessionId),
        cog: r.get::<_, Option<String>>("kind")?.and_then(|k| CrewKind::parse(&k)),
        question: r.get("question")?,
        raised_at: r.get("raised_at")?,
        reports: r.get("reports")?,
        ruling,
        settled_at: r.get("settled_at")?,
    })
}

impl CaseRepo for Ledger {
    fn open_case(&self, session: &SessionId) -> Result<Option<Case>> {
        let sql = format!("{CASE_SELECT} WHERE c.session_id = ?1 AND c.settled_at IS NULL");
        Ok(self.conn.query_row(&sql, params![session.0], row_to_case).optional()?)
    }

    fn case(&self, id: &CaseId) -> Result<Option<Case>> {
        Ok(self.conn.query_row(&format!("{CASE_SELECT} WHERE c.id = ?1"), params![id.0], row_to_case).optional()?)
    }

    fn cases(&self, convoy: Option<&ConvoyKey>, ticket: &TicketKey) -> Result<Vec<Case>> {
        let (sql, key) = match convoy {
            Some(c) => (format!("{CASE_SELECT} WHERE c.convoy = ?1 ORDER BY c.id DESC"), &c.0),
            None => (format!("{CASE_SELECT} WHERE c.convoy IS NULL AND c.ticket = ?1 ORDER BY c.id DESC"), &ticket.0),
        };
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params![key], row_to_case)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    fn verdict(&self, id: &CaseId) -> Result<Option<Inspection>> {
        Ok(self
            .conn
            .query_row("SELECT * FROM inspections WHERE case_id = ?1 ORDER BY id DESC LIMIT 1", params![id.0], row_to_inspection)
            .optional()?)
    }

    fn rule(&self, id: &CaseId, ruling: &Ruling) -> Result<()> {
        let case = self.case(id)?.ok_or_else(|| anyhow::anyhow!("no case #{}", id.0))?;
        anyhow::ensure!(case.ruling.is_none(), "case #{} is already ruled", id.0);
        let confirmed = self.verdict(id)?.is_some_and(|v| v.verdict == Verdict::Confirm);
        anyhow::ensure!(confirmed, "case #{} isn't confirmed: a captain inspects it first (/fleet-inspect)", id.0);
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "UPDATE cases SET ruling_question = ?2, ruling = ?3, ruled_at = ?4 WHERE id = ?1",
            params![id.0, ruling.question, ruling.answer, ruling.ruled_at],
        )?;
        ruled(&tx, id, ruling)?;
        tx.commit()?;
        Ok(())
    }

    fn rule_alone(&self, ticket: &TicketKey, ruling: &Ruling) -> Result<CaseId> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO cases(ticket, convoy, question, raised_at, ruling_question, ruling, ruled_at, settled_at)
             SELECT key, convoy, ?2, ?3, ?2, ?4, ?3, ?3 FROM tickets WHERE key = ?1",
            params![ticket.0, ruling.question, ruling.ruled_at, ruling.answer],
        )?;
        anyhow::ensure!(tx.changes() == 1, "{} isn't on the board", ticket.0);
        let id = CaseId(tx.last_insert_rowid());
        ruled(&tx, &id, ruling)?;
        tx.commit()?;
        Ok(id)
    }
}

/// The `EscalationRuled` event of a ruling, in its txn.
fn ruled(tx: &Connection, id: &CaseId, ruling: &Ruling) -> Result<()> {
    tx.execute(
        "INSERT INTO events(created_at, kind, entity_kind, entity_ref, payload_json) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            ruling.ruled_at,
            EventKind::EscalationRuled.as_str(),
            EntityKind::Escalation.as_str(),
            id.0.to_string(),
            serde_json::json!({ "question": ruling.question }).to_string(),
        ],
    )?;
    Ok(())
}

// ── proposals ───────────────────────────────────────────────────────────────────

fn row_to_proposal(r: &rusqlite::Row) -> rusqlite::Result<Proposal> {
    let proposed = match r.get::<_, Option<String>>("existing")? {
        Some(ticket) => Proposed::Existing {
            ticket: ticket_key(ticket)?,
            why: r.get("why")?,
            convoy: r.get::<_, Option<String>>("convoy")?.map(convoy_key).transpose()?,
        },
        None => {
            let blocked_by: Vec<String> = serde_json::from_str(&r.get::<_, String>("blocked_by_json")?)
                .map_err(|e| rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, e.into()))?;
            Proposed::New(TicketDraft {
                title: r.get("title")?,
                kind: r.get("kind")?,
                description: r.get("description")?,
                convoy: r.get::<_, Option<String>>("convoy")?.map(convoy_key).transpose()?,
                blocked_by: blocked_by.into_iter().map(ticket_key).collect::<rusqlite::Result<_>>()?,
                labels: vec![],
            })
        }
    };
    Ok(Proposal {
        id: ProposalId(r.get("id")?),
        from_ticket: r.get::<_, Option<String>>("from_ticket")?.map(ticket_key).transpose()?,
        proposed,
        created_at: r.get("created_at")?,
        ended_at: r.get("ended_at")?,
        accepted_as: r.get::<_, Option<String>>("accepted_as")?.map(ticket_key).transpose()?,
    })
}

impl ProposalRepo for Ledger {
    fn propose(&self, p: &Proposal) -> Result<ProposalId> {
        let from = p.from_ticket.as_ref().map(|k| &k.0);
        match &p.proposed {
            Proposed::New(d) => {
                let blocked_by = serde_json::to_string(&d.blocked_by.iter().map(|k| &k.0).collect::<Vec<_>>())?;
                self.conn.execute(
                    "INSERT INTO proposals(from_ticket, title, kind, description, convoy, blocked_by_json, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![from, d.title, d.kind, d.description, d.convoy.as_ref().map(|c| &c.0), blocked_by, p.created_at],
                )?
            }
            Proposed::Existing { ticket, why, convoy } => self.conn.execute(
                "INSERT INTO proposals(from_ticket, existing, why, convoy, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![from, ticket.0, why, convoy.as_ref().map(|c| &c.0), p.created_at],
            )?,
        };
        Ok(ProposalId(self.conn.last_insert_rowid()))
    }

    fn get(&self, id: &ProposalId) -> Result<Option<Proposal>> {
        Ok(self
            .conn
            .query_row("SELECT * FROM proposals WHERE id = ?1", params![id.0], row_to_proposal)
            .optional()?)
    }

    fn settle(&self, id: &ProposalId, at: &str, accepted_as: Option<&TicketKey>) -> Result<()> {
        let n = self.conn.execute(
            "UPDATE proposals SET ended_at = ?2, accepted_as = ?3 WHERE id = ?1 AND ended_at IS NULL",
            params![id.0, at, accepted_as.map(|k| &k.0)],
        )?;
        anyhow::ensure!(n == 1, "proposal {} isn't open", id.0);
        Ok(())
    }

    fn pending(&self) -> Result<Vec<Proposal>> {
        let mut stmt = self.conn.prepare("SELECT * FROM proposals WHERE ended_at IS NULL ORDER BY id")?;
        let rows = stmt.query_map([], row_to_proposal)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

// ── memory (the operator's standing decisions) ───────────────────────────────────

/// One memory entry as its row holds it: (id, kind, what, when, then, why, ruling).
type MemoryRow = (String, &'static str, Option<String>, Option<String>, Option<String>, String, String);

fn memory_rows(m: &Memory) -> Vec<MemoryRow> {
    let t = |s: &str| s.trim().to_string();
    let refused = m.refused.iter().map(|r| (t(&r.id), "refused", Some(t(&r.what)), None, None, t(&r.why), t(&r.ruling)));
    let rulings = m.rulings.iter().map(|r| (t(&r.id), "ruling", None, Some(t(&r.when)), Some(t(&r.then)), t(&r.why), t(&r.ruling)));
    refused.chain(rulings).collect()
}

impl MemoryRepo for Ledger {
    fn memory(&self) -> Result<Memory> {
        // Each entry where it was first set, so updating one doesn't move it.
        let mut stmt = self.conn.prepare(
            "SELECT kind, id, what, \"when\", \"then\", why, ruling FROM memory m WHERE ended_at IS NULL
             ORDER BY (SELECT MIN(seq) FROM memory f WHERE f.id = m.id)",
        )?;
        let mut rows = stmt.query([])?;
        let mut m = Memory::default();
        while let Some(r) = rows.next()? {
            let (kind, id, why, ruling): (String, String, String, String) = (r.get(0)?, r.get(1)?, r.get(5)?, r.get(6)?);
            match kind.as_str() {
                "refused" => m.refused.push(Refused { id, what: r.get(2)?, why, ruling }),
                _ => m.rulings.push(StandingRuling { id, when: r.get(3)?, then: r.get(4)?, why, ruling }),
            }
        }
        Ok(m)
    }

    fn remember(&self, m: &Memory, at: &str) -> Result<usize> {
        let tx = self.conn.unchecked_transaction()?;
        let mut set = 0;
        for row in memory_rows(m) {
            let live: Option<MemoryRow> = tx
                .query_row(
                    "SELECT id, kind, what, \"when\", \"then\", why, ruling FROM memory WHERE id = ?1 AND ended_at IS NULL",
                    params![row.0],
                    |r| Ok((r.get(0)?, if r.get::<_, String>(1)? == "refused" { "refused" } else { "ruling" }, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)),
                )
                .optional()?;
            if live.as_ref() == Some(&row) {
                continue;
            }
            tx.execute("UPDATE memory SET ended_at = ?2 WHERE id = ?1 AND ended_at IS NULL", params![row.0, at])?;
            tx.execute(
                "INSERT INTO memory(id, kind, what, \"when\", \"then\", why, ruling, set_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![row.0, row.1, row.2, row.3, row.4, row.5, row.6, at],
            )?;
            set += 1;
        }
        tx.commit()?;
        Ok(set)
    }

    fn forget(&self, id: &str, at: &str) -> Result<()> {
        let n = self.conn.execute("UPDATE memory SET ended_at = ?2 WHERE id = ?1 AND ended_at IS NULL", params![id, at])?;
        anyhow::ensure!(n == 1, "no entry {id:?} in the memory");
        Ok(())
    }
}

// ── events (the CDC change-log) ─────────────────────────────────────────────────

fn row_to_event(r: &rusqlite::Row) -> rusqlite::Result<Event> {
    let kind: String = r.get("kind")?;
    let entity_kind: String = r.get("entity_kind")?;
    let payload: String = r.get("payload_json")?;
    Ok(Event {
        seq: Seq(r.get("seq")?),
        created_at: r.get("created_at")?,
        kind: require(EventKind::parse(&kind), 2, "event kind")?,
        entity_kind: require(EntityKind::parse(&entity_kind), 3, "entity kind")?,
        entity_ref: r.get("entity_ref")?,
        payload: serde_json::from_str(&payload).unwrap_or(serde_json::Value::Null),
    })
}

impl EventLog for Ledger {
    fn append(
        &self,
        created_at: &str,
        kind: EventKind,
        entity: EntityKind,
        entity_ref: &str,
        payload: &serde_json::Value,
    ) -> Result<Seq> {
        let payload = serde_json::to_string(payload)?;
        self.conn.execute(
            "INSERT INTO events(created_at, kind, entity_kind, entity_ref, payload_json)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![created_at, kind.as_str(), entity.as_str(), entity_ref, payload],
        )?;
        Ok(Seq(self.conn.last_insert_rowid()))
    }

    fn since(&self, seq: Seq) -> Result<Vec<Event>> {
        let mut stmt = self.conn.prepare("SELECT * FROM events WHERE seq > ?1 ORDER BY seq")?;
        let rows = stmt.query_map(params![seq.0], row_to_event)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

#[cfg(test)]
mod repo_tests {
    use super::*;
    use super::tests::{bare_ticket, worker_on};

    fn db_with(key: &str) -> Ledger {
        let db = Ledger::in_memory().unwrap();
        TicketRepo::upsert(
            &db,
            &Ticket {
                key: TicketKey(key.into()),
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
        db
    }

    #[test]
    fn ship_and_snapshot_round_trip() {
        let db = db_with("PROJ-1");
        let ship = Ship {
            id: ShipId(0),
            ticket: Some(TicketKey("PROJ-1".into())),
            iid: 42,
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
        assert_eq!(db.armada().unwrap().len(), 1);

        let snap = ShipSnapshot {
            ship: id.clone(),
            title: "Add X".into(),
            state: ShipState::Open,
            head_sha: "abc".into(),
            has_conflicts: true,
            open_threads: 2,
            total_threads: 2,
            approvals: Approvals { current: 0, required: 1 },
            armed: false,
            on_merge_train: false,
            blocked_by: vec![],
            pipeline: Some(PipelineStatus::Failed),
            merge_status: None,
            synced_at: "t".into(),
        };
        db.sync_snapshot(&snap).unwrap();
        let got = db.snapshot(&id).unwrap().unwrap();
        assert_eq!(got.state, ShipState::Open);
        assert!(got.has_conflicts);
        assert_eq!(got.approvals, Approvals { current: 0, required: 1 });
        assert_eq!(got.pipeline, Some(PipelineStatus::Failed));

        // An allied ship (no ticket) also fits the one shape.
        let ally = Ship {
            id: ShipId(0), ticket: None, iid: 99, url: "u2".into(),
            target: "master".into(), source: String::new(), pilot: "ally".into(), created_at: "t".into(), ended_at: None,
            swept: None,
            reviewed: None,
            rounds: 0,
        };
        ShipRepo::upsert(&db, &ally).unwrap();
        assert_eq!(db.armada().unwrap().len(), 2);
    }

    #[test]
    fn order_round_trips_and_cancels() {
        let db = db_with("PROJ-1");
        let order = Order {
            ticket: TicketKey("PROJ-1".into()),
            auto_impl: true,
            fork_from: "origin/master".into(),
            ac_summary: "do the thing".into(),
            park_at: None,
            worktree_path: Some("/wt/PROJ-1".into()),
            created_at: "t".into(),
        };
        db.dispatch(&order).unwrap();
        let got = OrderRepo::get(&db, &TicketKey("PROJ-1".into())).unwrap().unwrap();
        assert!(got.auto_impl);
        assert_eq!(db.queue().unwrap().len(), 1);

        // dispatch emits a Dispatched event (CDC) keyed by the ticket.
        let ev = db.since(Seq(0)).unwrap();
        let d = ev.iter().find(|e| e.kind == EventKind::Dispatched).expect("a Dispatched event");
        assert_eq!(d.entity_kind, EntityKind::Ticket);
        assert_eq!(d.entity_ref, "PROJ-1");
        assert_eq!(d.payload["auto_impl"], true);

        db.cancel(&TicketKey("PROJ-1".into())).unwrap();
        assert!(db.queue().unwrap().is_empty());
    }

    #[test]
    fn an_inspection_is_recorded_and_the_latest_counts() {
        let db = db_with("PROJ-1");
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
        let id = db.start(&worker).unwrap().unwrap();
        assert_eq!(db.inspection(&id).unwrap(), None, "never inspected");
        let at = |reported: &str, verdict| Inspection {
            session: id.clone(),
            case: None,
            reported_at: reported.into(),
            verdict,
            body: "**Verdict:** …".into(),
            inspected_at: "t9".into(),
        };
        db.inspect(&at("t1", Verdict::Refute)).unwrap();
        db.inspect(&at("t2", Verdict::Confirm)).unwrap();
        let got = db.inspection(&id).unwrap().unwrap();
        assert_eq!((got.reported_at.as_str(), got.verdict), ("t2", Verdict::Confirm));
        assert!(db.inspect(&Inspection { body: " ".into(), ..at("t3", Verdict::Confirm) }).is_err(), "a verdict says why");
    }

    fn escalate(db: &Ledger, session: &SessionId, question: &str, at: &str) {
        let state = CogState::Worker(WorkerState::Escalated);
        db.record(&Report { session: session.clone(), state, last_action: Some(question.into()), reported_at: at.into() }).unwrap();
    }

    fn verdict_on(db: &Ledger, case: &Case, verdict: Verdict) {
        let session = case.session.clone().unwrap();
        db.inspect(&Inspection { session, case: Some(case.id.clone()), reported_at: "t".into(), verdict, body: "**Verdict:** x".into(), inspected_at: "t".into() })
            .unwrap();
    }

    #[test]
    fn an_escalation_awaiting_its_answer_takes_every_rereport_and_an_answered_one_takes_none() {
        let db = db_with("PROJ-1");
        let id = db.start(&worker_on("PROJ-1")).unwrap().unwrap();
        escalate(&db, &id, "  needs a ruling on X ", "t1");
        let case = db.open_case(&id).unwrap().expect("raised");
        assert_eq!((case.question.as_str(), case.reports, case.cog), ("needs a ruling on X", 1, Some(CrewKind::Worker)));
        escalate(&db, &id, "relaunched; still awaiting a ruling on X", "t2"); // a restart's re-report, reworded
        verdict_on(&db, &case, Verdict::Confirm);
        escalate(&db, &id, "still waiting", "t3"); // confirmed is not answered: the ruling hasn't come
        let joined = db.open_case(&id).unwrap().unwrap();
        assert_eq!((joined.id.clone(), joined.reports, joined.question.as_str()), (case.id.clone(), 3, "needs a ruling on X"), "one case, its question kept");
        assert!(db.verdict(&joined.id).unwrap().is_some_and(|v| v.judges(&joined)), "still judged");

        let ruling = Ruling { question: "X?".into(), answer: "Do Y.".into(), ruled_at: "t4".into() };
        db.rule(&case.id, &ruling).unwrap();
        escalate(&db, &id, "needs a ruling on X", "t5"); // answered: the same words are a new case
        let next = db.open_case(&id).unwrap().unwrap();
        assert_ne!(next.id, case.id, "the ruling didn't hold: a new case");
        assert_eq!(db.case(&case.id).unwrap().unwrap().settled_at.as_deref(), Some("t5"));
        assert_eq!(db.verdict(&next.id).unwrap(), None, "unjudged until it's inspected");

        verdict_on(&db, &next, Verdict::Refute);
        escalate(&db, &id, "the way out failed", "t6");
        let third = db.open_case(&id).unwrap().unwrap();
        assert_ne!(third.id, next.id, "a REFUTE answered it too: a new case");

        let state = CogState::Worker(WorkerState::Implementing);
        db.record(&Report { session: id.clone(), state, last_action: None, reported_at: "t7".into() }).unwrap();
        assert_eq!(db.open_case(&id).unwrap(), None, "moved on: settled");
        let all = db.cases(None, &TicketKey("PROJ-1".into())).unwrap();
        assert_eq!(all.iter().map(|c| c.id.clone()).collect::<Vec<_>>(), vec![third.id, next.id, case.id], "newest first");
    }

    #[test]
    fn ending_the_session_settles_its_open_case() {
        let db = db_with("PROJ-1");
        let id = db.start(&worker_on("PROJ-1")).unwrap().unwrap();
        escalate(&db, &id, "stuck", "t1");
        db.end(&id, "t2").unwrap();
        assert_eq!(db.open_case(&id).unwrap(), None);
        assert_eq!(db.cases(None, &TicketKey("PROJ-1".into())).unwrap()[0].settled_at.as_deref(), Some("t2"));
    }

    #[test]
    fn a_case_is_ruled_once_confirmed_and_a_ruling_alone_is_its_own_case() {
        let db = db_with("PROJ-1");
        let id = db.start(&worker_on("PROJ-1")).unwrap().unwrap();
        escalate(&db, &id, "which auth?", "t1");
        let case = db.open_case(&id).unwrap().unwrap();
        let ruling = Ruling { question: "Which auth does an admin route take?".into(), answer: "Rev 4's".into(), ruled_at: "t5".into() };
        assert!(db.rule(&case.id, &ruling).is_err(), "not inspected yet");
        verdict_on(&db, &case, Verdict::Refute);
        assert!(db.rule(&case.id, &ruling).is_err(), "refuted: the cog's own way out, no ruling");
        verdict_on(&db, &case, Verdict::Confirm);
        db.rule(&case.id, &ruling).unwrap();
        assert!(db.rule(&case.id, &ruling).is_err(), "ruled once");
        let ruled = db.case(&case.id).unwrap().unwrap();
        assert_eq!((ruled.ruling.as_ref(), ruled.settled_at.as_ref()), (Some(&ruling), None), "it settles when its cog moves on");
        assert_eq!(ruled.outcome(Some(Verdict::Confirm)), "ruled");

        let alone = db.rule_alone(&TicketKey("PROJ-1".into()), &ruling).unwrap();
        let alone = db.case(&alone).unwrap().unwrap();
        assert_eq!((alone.session, alone.settled_at.as_deref(), alone.question.as_str()), (None, Some("t5"), ruling.question.as_str()));
        assert!(db.rule_alone(&TicketKey("PROJ-404".into()), &ruling).is_err(), "not on the board");
        assert_eq!(db.events_of(EventKind::EscalationRuled).unwrap().len(), 2);
    }

    #[test]
    fn a_v8_ledger_turns_its_judged_and_standing_escalations_into_cases() {
        let dir = std::env::temp_dir().join(format!("ledger-migrate-v9-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("ledger.db").to_string_lossy().into_owned();
        let db = Ledger::open(&path).unwrap();
        TicketRepo::upsert(&db, &bare_ticket("PROJ-1")).unwrap();
        TicketRepo::upsert(&db, &bare_ticket("PROJ-2")).unwrap();
        let (one, two) = (db.start(&worker_on("PROJ-1")).unwrap().unwrap(), db.start(&worker_on("PROJ-2")).unwrap().unwrap());
        drop(db);
        // v8: reports and inspections, no cases. PROJ-1 was judged twice on one question (a heal's
        // re-report), then once on another it still stands on; PROJ-2 stands unjudged.
        let conn = Connection::open(&path).unwrap();
        let report = |s: &SessionId, q: &str, at: &str| {
            format!(
                "INSERT INTO events(created_at, kind, entity_kind, entity_ref, payload_json)
                 VALUES ('{at}', 'status_reported', 'session', '{}', json_object('state', 'escalated', 'last_action', '{q}'));",
                s.0
            )
        };
        let judged = |s: &SessionId, at: &str, v: &str| {
            format!("INSERT INTO inspections(session_id, reported_at, verdict, body, inspected_at) VALUES ({}, '{at}', '{v}', 'b', '{at}9');", s.0)
        };
        conn.execute_batch(&format!(
            "DELETE FROM cases; DROP INDEX one_open_case; ALTER TABLE inspections DROP COLUMN case_id; ALTER TABLE ship_snapshots DROP COLUMN threads_changed_at; DROP INDEX convoy_no; ALTER TABLE convoys DROP COLUMN no; ALTER TABLE convoys DROP COLUMN goal; ALTER TABLE convoys DROP COLUMN standing; DROP TABLE cases;
             {} {} {} {} {} {} {}
             INSERT INTO reports(session_id, state, last_action, reported_at) VALUES ({}, 'escalated', 'Y?', 't3'), ({}, 'escalated', 'Z?', 't4');
             PRAGMA user_version = 8;",
            report(&one, "X?", "t1"), report(&one, "X?", "t2"), report(&one, "Y?", "t3"),
            judged(&one, "t1", "confirm"), judged(&one, "t2", "confirm"), judged(&one, "t3", "refute"), report(&two, "Z?", "t4"),
            one.0, two.0,
        ))
        .unwrap();
        drop(conn);

        let db = Ledger::open(&path).unwrap();
        let ones = db.cases(None, &TicketKey("PROJ-1".into())).unwrap();
        let shape: Vec<_> = ones.iter().map(|c| (c.question.as_str(), c.reports, c.settled_at.is_some())).collect();
        assert_eq!(shape, vec![("Y?", 1, false), ("X?", 2, true)], "one case per question; the standing one open");
        assert_eq!(db.verdict(&ones[1].id).unwrap().map(|v| v.verdict), Some(Verdict::Confirm), "its verdicts attached");
        assert!(db.verdict(&ones[0].id).unwrap().is_some_and(|v| v.judges(&ones[0])), "the standing case stays judged");
        let standing = db.open_case(&two).unwrap().expect("PROJ-2's escalation is a case");
        assert_eq!((standing.question.as_str(), db.verdict(&standing.id).unwrap()), ("Z?", None));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_convoy_takes_the_next_number_and_keeps_it_when_reopened() {
        let db = Ledger::in_memory().unwrap();
        let (a, b) = (ConvoyKey("PROJ-100".into()), ConvoyKey("PROJ-200".into()));
        db.open(&a, None, false, "t1").unwrap();
        db.open(&b, Some("Second"), false, "t2").unwrap();
        db.close(&a, "t3").unwrap();
        db.open(&a, Some("Again"), false, "t4").unwrap();
        let no = |k: &ConvoyKey| db.active().unwrap().into_iter().find(|c| &c.key == k).unwrap().no;
        assert_eq!((no(&a), no(&b)), (1, 2), "reopened, it keeps #1");
        assert_eq!((db.convoy_by_no(2).unwrap(), db.convoy_by_no(3).unwrap()), (Some(b), None));
        db.open(&ConvoyKey("PROJ-300".into()), None, false, "t5").unwrap();
        assert_eq!(db.convoy_by_no(3).unwrap(), Some(ConvoyKey("PROJ-300".into())), "never reused: the next is #3");
        db.open(&ConvoyKey("PROJ-050".into()), None, false, "t6").unwrap();
        let order: Vec<i64> = db.active().unwrap().iter().map(|c| c.no).collect();
        assert_eq!(order, [1, 2, 3, 4], "listed by number, not by key");
    }

    #[test]
    fn proposal_round_trips_then_closes_once() {
        let db = db_with("PROJ-1");
        let draft = TicketDraft {
            title: "Retry the flaky upload".into(),
            kind: "bug".into(),
            description: "## Context\n…".into(),
            convoy: Some(ConvoyKey("PROJ-9".into())),
            blocked_by: vec![TicketKey("PROJ-1".into())],
            labels: vec![],
        };
        let proposal = |proposed| Proposal {
            id: ProposalId(0),
            from_ticket: Some(TicketKey("PROJ-1".into())),
            proposed,
            created_at: "t".into(),
            ended_at: None,
            accepted_as: None,
        };
        let existing = Proposed::Existing {
            ticket: TicketKey("PROJ-1".into()),
            why: "blocks the merge".into(),
            convoy: Some(ConvoyKey("PROJ-9".into())),
        };
        let id = db.propose(&proposal(Proposed::New(draft.clone()))).unwrap();
        db.propose(&proposal(existing.clone())).unwrap();
        let open = db.pending().unwrap();
        assert_eq!(open.iter().map(|p| p.proposed.clone()).collect::<Vec<_>>(), vec![Proposed::New(draft), existing]);
        assert_eq!(open[0].from_ticket, Some(TicketKey("PROJ-1".into())));

        db.settle(&id, "t2", Some(&TicketKey("PROJ-10".into()))).unwrap();
        assert_eq!(db.pending().unwrap().len(), 1);
        let filed = ProposalRepo::get(&db, &id).unwrap().unwrap();
        assert_eq!(filed.accepted_as, Some(TicketKey("PROJ-10".into())));
        assert!(db.settle(&id, "t3", None).is_err(), "a settled proposal can't be settled again");

        // The schema allows exactly one shape: a draft with a title, or an existing ticket with a why.
        for bad in [
            "INSERT INTO proposals(created_at) VALUES ('t')",
            "INSERT INTO proposals(existing, created_at) VALUES ('PROJ-1', 't')",
            "INSERT INTO proposals(existing, why, title, created_at) VALUES ('PROJ-1', 'w', 'x', 't')",
        ] {
            assert!(db.conn.execute(bad, []).is_err(), "accepted: {bad}");
        }
    }

    #[test]
    fn events_append_and_follow_by_cursor() {
        let db = Ledger::in_memory().unwrap();
        let s1 = db
            .append("t", EventKind::TickRan, EntityKind::Ticker, "reconcile",
                    &serde_json::json!({"before": 1, "after": 1}))
            .unwrap();
        let s2 = db
            .append("t", EventKind::ShipMerged, EntityKind::Ship, "42", &serde_json::json!({}))
            .unwrap();
        assert!(s2.0 > s1.0, "seq is monotonic");
        // Follow from the first cursor → only the second event.
        let after = db.since(s1.clone()).unwrap();
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].kind, EventKind::ShipMerged);
        assert_eq!(after[0].entity_kind, EntityKind::Ship);
        // TickRan payload carries the version-span (no-op: before==after).
        let all = db.since(Seq(0)).unwrap();
        assert_eq!(all[0].payload["before"], all[0].payload["after"]);
    }

    #[test]
    fn tracker_sync_updates_cache_but_preserves_held_and_created_at() {
        let db = Ledger::in_memory().unwrap();
        // An existing ticket we've marked held, with our own created_at.
        TicketRepo::upsert(
            &db,
            &Ticket {
                key: TicketKey("PROJ-1".into()),
                convoy: None,
                held: true,
                created_at: "orig".into(),
                blocked_by: vec![],
                title: Some("old title".into()),
                assignee: None,
                status: Some(TicketStatus::Todo),
                synced_at: None,
            },
        )
        .unwrap();

        // A tracker sync refreshes the cache fields...
        db.sync(
            &TrackerTicket {
                key: TicketKey("PROJ-1".into()),
                title: "new title".into(),
                status: TicketStatus::InProgress,
                assignee: Some("me".into()),
                convoy: None,
                blocked_by: vec![],
                labels: vec![],
            },
            "t2",
        )
        .unwrap();

        let got = TicketRepo::get(&db, &TicketKey("PROJ-1".into())).unwrap().unwrap();
        assert_eq!(got.title.as_deref(), Some("new title")); // cache updated
        assert_eq!(got.status, Some(TicketStatus::InProgress));
        assert_eq!(got.assignee.as_deref(), Some("me"));
        assert!(got.held, "held must be preserved across a tracker sync");
        assert_eq!(got.created_at, "orig", "created_at must be preserved");

        // A sync for an unseen ticket inserts it (held defaults false).
        db.sync(
            &TrackerTicket {
                key: TicketKey("PROJ-2".into()),
                title: "fresh".into(),
                status: TicketStatus::Todo,
                assignee: None,
                convoy: None,
                blocked_by: vec![],
                labels: vec![],
            },
            "t3",
        )
        .unwrap();
        let fresh = TicketRepo::get(&db, &TicketKey("PROJ-2".into())).unwrap().unwrap();
        assert!(!fresh.held);
        assert_eq!(fresh.created_at, "t3");
    }

    #[test]
    fn sync_wires_known_blockers_skips_dangling_and_clears_removed() {
        let db = Ledger::in_memory().unwrap();
        let tt = |key: &str, blocked_by: Vec<&str>| TrackerTicket {
            key: TicketKey(key.into()),
            title: "T".into(),
            status: TicketStatus::Todo,
            assignee: None,
            convoy: None,
            blocked_by: blocked_by.into_iter().map(|b| TicketKey(b.into())).collect(),
            labels: vec![],
        };
        // PROJ-2 is tracked; PROJ-99 is not. PROJ-1 claims both as blockers.
        db.sync(&tt("PROJ-2", vec![]), "t").unwrap();
        db.sync(&tt("PROJ-1", vec!["PROJ-2", "PROJ-99"]), "t").unwrap();
        assert_eq!(
            TicketRepo::get(&db, &TicketKey("PROJ-1".into())).unwrap().unwrap().blocked_by,
            vec![TicketKey("PROJ-2".into())],
            "known blocker wired; dangling PROJ-99 skipped (no FK trip)"
        );
        // Re-sync PROJ-1 with no blockers → the stale edge clears (Jira is authoritative).
        db.sync(&tt("PROJ-1", vec![]), "t2").unwrap();
        assert!(
            TicketRepo::get(&db, &TicketKey("PROJ-1".into())).unwrap().unwrap().blocked_by.is_empty(),
            "an emptied blocked_by clears the stale edge"
        );
    }

    #[test]
    fn settings_kv_roundtrips_and_overwrites() {
        let db = Ledger::in_memory().unwrap();
        assert_eq!(db.setting_get("poll_secs").unwrap(), None, "absent by default");
        db.setting_set("poll_secs", "9").unwrap();
        assert_eq!(db.setting_get("poll_secs").unwrap().as_deref(), Some("9"));
        db.setting_set("poll_secs", "12").unwrap(); // upsert overwrites
        assert_eq!(db.setting_get("poll_secs").unwrap().as_deref(), Some("12"));
    }
}
