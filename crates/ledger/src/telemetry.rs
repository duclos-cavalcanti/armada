//! telemetry — the fleet's cost/health store (PLAN §telemetry). A SEPARATE SQLite DB from the
//! operational ledger: telemetry is append-heavy (a row per cog `api_request`), so it stays off
//! the ledger's single-writer path and can't contend with reconcile/dispatch. fleetd's OTLP
//! receiver writes rows here; the read-side rollups consume them (optionally via DuckDB ATTACH).
//!
//! Logs-first: a row is one Claude Code log event (the granular `cost_usd`/tokens live on
//! `claude_code.api_request`). The hot facts (cost, model, attribution) are columns; the full
//! attribute bag stays in `attrs_json`, so a new Claude attribute never needs a migration.

use anyhow::Result;
use rusqlite::{params, Connection};

const SCHEMA: &str = "\
CREATE TABLE IF NOT EXISTS telemetry (
    id          INTEGER PRIMARY KEY,
    received_at TEXT NOT NULL,   -- fleetd ingest time (RFC3339)
    ts          TEXT,            -- the OTEL record time (RFC3339), when present
    event       TEXT NOT NULL,   -- log event name (body.stringValue), e.g. claude_code.api_request
    service     TEXT,            -- resource service.name (mady-<role>)
    role        TEXT,            -- from the mady.role resource attribute
    ticket      TEXT,            -- from the mady.ticket resource attribute
    session_id  TEXT,            -- Claude's session.id
    cost_usd    REAL,            -- hot fact: per-request cost
    model       TEXT,            -- hot dimension
    attrs_json  TEXT NOT NULL    -- the remaining attributes (those not promoted to columns)
);
-- Access path: SUM(cost_usd) GROUP BY role[, ticket] over a time window (the rollup query).
CREATE INDEX IF NOT EXISTS telemetry_by_role_ticket ON telemetry(role, ticket);
CREATE INDEX IF NOT EXISTS telemetry_by_ts ON telemetry(ts);
";

/// One ingested telemetry record (logs-first — a Claude Code log event).
#[derive(Debug, Clone, PartialEq)]
pub struct TelemetryRow {
    pub received_at: String,
    pub ts: Option<String>,
    pub event: String,
    pub service: Option<String>,
    pub role: Option<String>,
    pub ticket: Option<String>,
    pub session_id: Option<String>,
    pub cost_usd: Option<f64>,
    pub model: Option<String>,
    pub attrs_json: String,
}

/// The telemetry store — a dedicated SQLite DB, separate from the operational ledger.
pub struct TelemetryStore {
    conn: Connection,
}

impl TelemetryStore {
    /// Open (or create) the telemetry DB at `path`: WAL mode, schema applied.
    pub fn open(path: &str) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch(SCHEMA)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        Ok(Self { conn })
    }

    /// An in-memory store for tests.
    pub fn in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self { conn })
    }

    /// Open read-only (no schema write, no WAL change) — for consumers like `fleetc cost`.
    /// Errors if the DB doesn't exist yet (nothing has been collected).
    pub fn open_readonly(path: &str) -> Result<Self> {
        let conn = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        Ok(Self { conn })
    }

    /// Append one telemetry row; returns its rowid.
    pub fn record(&self, row: &TelemetryRow) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO telemetry
               (received_at, ts, event, service, role, ticket, session_id, cost_usd, model, attrs_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                row.received_at, row.ts, row.event, row.service, row.role,
                row.ticket, row.session_id, row.cost_usd, row.model, row.attrs_json,
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Cost rolled up by role for records at/after `since` (RFC3339) — the hot read the dashboard
    /// wants. A one-off `claude` (no attribution) has a NULL role and rolls up under `interactive`.
    /// Sorted by spend, descending.
    pub fn cost_by_role(&self, since: &str) -> Result<Vec<(String, f64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT COALESCE(role, 'interactive') AS r, SUM(cost_usd) AS c
             FROM telemetry
             WHERE cost_usd IS NOT NULL AND ts >= ?1
             GROUP BY r
             ORDER BY c DESC",
        )?;
        let rows = stmt.query_map(params![since], |row| Ok((row.get(0)?, row.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(role: &str, ticket: &str, ts: &str, cost: f64) -> TelemetryRow {
        TelemetryRow {
            received_at: "r".into(),
            ts: Some(ts.into()),
            event: "claude_code.api_request".into(),
            service: Some(format!("mady-{role}")),
            role: Some(role.into()),
            ticket: Some(ticket.into()),
            session_id: Some("s1".into()),
            cost_usd: Some(cost),
            model: Some("claude-opus".into()),
            attrs_json: "{}".into(),
        }
    }

    #[test]
    fn records_a_row_and_returns_its_id() {
        let db = TelemetryStore::in_memory().unwrap();
        let id = db.record(&row("worker", "MADY-1", "2026-10-13T00:00:00Z", 0.5)).unwrap();
        assert!(id > 0);
    }

    #[test]
    fn cost_by_role_sums_within_the_window_and_sorts_by_spend() {
        let db = TelemetryStore::in_memory().unwrap();
        db.record(&row("worker", "MADY-1", "2026-10-13T01:00:00Z", 1.0)).unwrap();
        db.record(&row("worker", "MADY-2", "2026-10-13T02:00:00Z", 2.0)).unwrap();
        db.record(&row("sailor", "MADY-1", "2026-10-13T03:00:00Z", 0.5)).unwrap();
        // older than the window → excluded.
        db.record(&row("worker", "MADY-9", "2026-10-12T00:00:00Z", 99.0)).unwrap();

        let got = db.cost_by_role("2026-10-13T00:00:00Z").unwrap();
        assert_eq!(got, vec![("worker".to_string(), 3.0), ("sailor".to_string(), 0.5)]);
    }

    #[test]
    fn a_null_role_rolls_up_under_interactive() {
        let db = TelemetryStore::in_memory().unwrap();
        db.record(&row("worker", "MADY-1", "2026-10-14T01:00:00Z", 1.0)).unwrap();
        let mut one_off = row("x", "x", "2026-10-14T02:00:00Z", 2.0); // a bare `claude`
        one_off.role = None;
        one_off.ticket = None;
        db.record(&one_off).unwrap();

        let got = db.cost_by_role("2026-10-14T00:00:00Z").unwrap();
        assert_eq!(got, vec![("interactive".to_string(), 2.0), ("worker".to_string(), 1.0)]);
    }
}
