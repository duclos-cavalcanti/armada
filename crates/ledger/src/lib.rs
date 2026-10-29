//! fleet ledger — the SQLite persistence adapter (PLAN §3.1).
//!
//! `fleetd` is the single writer. This crate owns the schema (the integrity layer:
//! NOT NULL / CHECK / FK) and the row↔domain mapping behind the repository ports.
//! `domain` stays sqlite-free; the enum ↔ TEXT mapping uses `domain`'s `as_str`/
//! `parse` helpers.

use anyhow::Result;
use domain::ports::{
    ConvoyRepo, EscalationRepo, EventLog, LookoutRepo, OrderRepo, ProposalRepo, ReportRepo, SessionRepo,
    ShipRepo, TicketRepo,
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
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.execute_batch(SCHEMA)?;
    stamp(conn, SCHEMA_VERSION, "ledger")
}

/// The ledger's schema version (SQLite `user_version`). There are no migrations: bump this when
/// an existing table in `schema.sql` changes shape, so a ledger or an export from another version
/// is refused by name instead of failing later on a missing column. A new table needs no bump:
/// `CREATE TABLE IF NOT EXISTS` adds it to an older ledger the next time fleetd opens it.
pub const SCHEMA_VERSION: i64 = 1;

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
    })
}

impl SessionRepo for Ledger {
    fn start(&self, s: &Session) -> Result<Option<SessionId>> {
        let res = self.conn.execute(
            "INSERT INTO sessions(kind, ticket, convoy, worktree_path, tmux_session, harness_session_id, created_at, ended_at, anchored_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
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
            ],
        );
        match res {
            Ok(_) => Ok(Some(SessionId(self.conn.last_insert_rowid()))),
            // A live-spawn lock (one_live_crew_per_ticket or one_live_captain_per_convoy)
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
        self.conn
            .execute("UPDATE sessions SET ended_at = ?2 WHERE id = ?1", params![id.0, at])?;
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
        assert!(e.contains("schema v2") && e.contains("expects v1"), "{e}");
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

    fn bare_ticket(key: &str) -> Ticket {
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

    fn worker_on(key: &str) -> Session {
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
        }
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
        db.open(&ConvoyKey("PROJ-100".into()), Some("auth"), "t1").unwrap();
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
    fn convoy_reopen_clears_ended_at() {
        let db = Ledger::in_memory().unwrap();
        let k = ConvoyKey("PROJ-100".into());
        db.open(&k, None, "t1").unwrap();
        db.close(&k, "t2").unwrap();
        db.open(&k, Some("revived"), "t3").unwrap();
        let active = db.active().unwrap();
        assert_eq!(active.len(), 1, "re-opening clears ended_at");
        assert_eq!(active[0].title.as_deref(), Some("revived"));
    }

    #[test]
    fn convoy_open_and_close_log_cdc_events_and_a_noop_close_logs_nothing() {
        let db = Ledger::in_memory().unwrap();
        let k = ConvoyKey("PROJ-100".into());
        db.open(&k, None, "t1").unwrap();
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
        approvals: Approvals {
            current: r.get("approvals_current")?,
            required: r.get("approvals_required")?,
        },
        armed: r.get::<_, i64>("armed")? != 0,
        on_merge_train: r.get::<_, i64>("on_merge_train")? != 0,
        pipeline,
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
            "INSERT INTO ship_snapshots(ship_id, title, state, head_sha, has_conflicts, open_threads,
                 approvals_current, approvals_required, armed, on_merge_train, pipeline, merge_status, synced_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
             ON CONFLICT(ship_id) DO UPDATE SET
               title=excluded.title, state=excluded.state, head_sha=excluded.head_sha,
               has_conflicts=excluded.has_conflicts, open_threads=excluded.open_threads,
               approvals_current=excluded.approvals_current, approvals_required=excluded.approvals_required,
               armed=excluded.armed, on_merge_train=excluded.on_merge_train, pipeline=excluded.pipeline,
               merge_status=excluded.merge_status, synced_at=excluded.synced_at",
            params![
                s.ship.0, s.title, s.state.as_str(), s.head_sha, s.has_conflicts as i64,
                s.open_threads, s.approvals.current, s.approvals.required, s.armed as i64,
                s.on_merge_train as i64, s.pipeline.map(|p| p.as_str()), s.merge_status, s.synced_at,
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

    fn set_reviewed(&self, id: &ShipId, head_sha: &str) -> Result<()> {
        self.conn
            .execute("UPDATE ships SET reviewed = ?2 WHERE id = ?1", params![id.0, head_sha])?;
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
        title: r.get("title")?,
        created_at: r.get("created_at")?,
        ended_at: r.get("ended_at")?,
        anchored_at: r.get("anchored_at")?,
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

    fn open(&self, key: &ConvoyKey, title: Option<&str>, now: &str) -> Result<()> {
        // Idempotent create; re-opening a closed convoy clears ended_at. +ConvoyOpened (CDC).
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO convoys(key, title, created_at, ended_at)
             VALUES (?1, ?2, ?3, NULL)
             ON CONFLICT(key) DO UPDATE SET title = excluded.title, ended_at = NULL",
            params![key.0, title, now],
        )?;
        tx.execute(
            "INSERT INTO events(created_at, kind, entity_kind, entity_ref, payload_json)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                now,
                EventKind::ConvoyOpened.as_str(),
                EntityKind::Convoy.as_str(),
                key.0,
                serde_json::to_string(&serde_json::json!({ "title": title }))?,
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

    fn active(&self) -> Result<Vec<Convoy>> {
        let mut stmt = self.conn.prepare("SELECT * FROM convoys WHERE ended_at IS NULL ORDER BY key")?;
        let rows = stmt.query_map([], row_to_convoy)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

// ── lookout (the singleton allied-ship reviewer's one control row) ──────────────────────

impl LookoutRepo for Ledger {
    fn anchor(&self, at: &str) -> Result<()> {
        // Set anchored_at on the singleton row + LookoutAnchored (CDC, same txn). No-op logs nothing.
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

// ── escalations ─────────────────────────────────────────────────────────────────

fn row_to_escalation(r: &rusqlite::Row) -> rusqlite::Result<Escalation> {
    let raised: String = r.get("raised_by")?;
    let evidence_json: String = r.get("evidence_json")?;
    Ok(Escalation {
        id: EscalationId(r.get("id")?),
        ticket: ticket_key(r.get("ticket")?)?,
        raised_by: require(CrewKind::parse(&raised), 2, "raised_by")?,
        summary: r.get("summary")?,
        evidence: serde_json::from_str(&evidence_json).unwrap_or_default(),
        ship_iid: r.get("ship_iid")?,
        phase: r.get("phase")?,
        created_at: r.get("created_at")?,
        ended_at: r.get("ended_at")?,
        resolution: r.get("resolution")?,
    })
}

impl EscalationRepo for Ledger {
    fn raise(&self, e: &Escalation) -> Result<EscalationId> {
        let evidence = serde_json::to_string(&e.evidence)?;
        self.conn.execute(
            "INSERT INTO escalations(ticket, raised_by, summary, evidence_json, ship_iid, phase, created_at, ended_at, resolution)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                e.ticket.0, e.raised_by.as_str(), e.summary, evidence,
                e.ship_iid, e.phase, e.created_at, e.ended_at, e.resolution,
            ],
        )?;
        Ok(EscalationId(self.conn.last_insert_rowid()))
    }

    fn resolve(&self, id: &EscalationId, at: &str, resolution: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE escalations SET ended_at = ?2, resolution = ?3 WHERE id = ?1",
            params![id.0, at, resolution],
        )?;
        Ok(())
    }

    fn list_open(&self) -> Result<Vec<Escalation>> {
        let mut stmt = self
            .conn
            .prepare("SELECT * FROM escalations WHERE ended_at IS NULL ORDER BY id")?;
        let rows = stmt.query_map([], row_to_escalation)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
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
            approvals: Approvals { current: 0, required: 1 },
            armed: false,
            on_merge_train: false,
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
    fn escalation_raise_resolve() {
        let db = db_with("PROJ-1");
        let esc = Escalation {
            id: EscalationId(0),
            ticket: TicketKey("PROJ-1".into()),
            raised_by: CrewKind::Worker,
            summary: "blocked on X".into(),
            evidence: vec!["auth.rs:407".into(), "E0050".into()],
            ship_iid: Some(42),
            phase: Some("impl".into()),
            created_at: "t".into(),
            ended_at: None,
            resolution: None,
        };
        let id = db.raise(&esc).unwrap();
        let open = db.list_open().unwrap();
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].evidence, vec!["auth.rs:407".to_string(), "E0050".to_string()]);
        db.resolve(&id, "t2", "wired the edge").unwrap();
        assert!(db.list_open().unwrap().is_empty());
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
