//! The fleet view model — a read-only snapshot of the ledger for the dashboard.
//!
//! Reloaded every refresh tick. The ledger is the single source of truth; this is
//! just a frozen read built from the typed repos, so the dashboard mirrors no schema
//! (that was the bash kit's `types.rs`). Opens the ledger READ-ONLY — a concurrent
//! reader beside fleetd's single writer (SQLite WAL), exactly like `fleetc plan`.
//!
//! Liveness note: the `Active/Quiet/Dead` freshness classification isn't in the
//! ledger (fleetd derives it in-memory from tmux probes). The proxy here is report
//! AGE — `now − reported_at` — which catches a stalled cog without re-probing tmux.

use domain::ports::{
    ConvoyRepo, EscalationRepo, OrderRepo, ReportRepo, SessionRepo, ShipRepo, TicketRepo,
};
use domain::{Convoy, CrewKind, Escalation, Event, Order, Session, Ship, ShipSnapshot, Ticket};
use ledger::Ledger;
use std::path::{Path, PathBuf};

/// One live cog, projected for the roster: its session, its latest self-report (for
/// the reporting kinds — worker/sailor/hand; `None` for the report-less captain/lookout),
/// the age of that report in seconds, and fleetd's live tmux-probe liveness
/// (`Active/Quiet/Idle/Dead`) when the daemon is reachable (`None` otherwise → fall
/// back to report-age).
pub struct CogRow {
    pub session: Session,
    pub state: Option<String>,
    pub last_action: Option<String>,
    pub age_secs: Option<i64>,
    pub liveness: Option<String>,
}

/// A ship paired with its latest snapshot (if the armada sync has populated one) and
/// its most-recent sailor's last self-report. `sailor_state` is durable past the reap:
/// the cold loop ends a swept sailor's session, but its `reports` row survives, so a
/// ship at rest still shows `sleeping` here (= attended, idle) with no live cog.
pub struct ShipRow {
    pub ship: Ship,
    pub snapshot: Option<ShipSnapshot>,
    pub sailor_state: Option<String>,
}

/// The whole dashboard read — one atomic snapshot of the ledger. `load_error` is set
/// (and the rest left empty) when the ledger can't be opened — the normal cold-start
/// state before fleetd has created it.
pub struct Fleet {
    pub ledger_path: PathBuf,
    pub load_error: Option<String>,
    pub cogs: Vec<CogRow>,
    pub board: Vec<Ticket>,
    pub armada: Vec<ShipRow>,
    pub convoys: Vec<Convoy>,
    pub escalations: Vec<Escalation>,
    pub orders: Vec<Order>,
    /// The latest reconcile `TickRan` event — fleetd's last plan (applied/failed/planned).
    pub tick: Option<Event>,
}

impl Fleet {
    /// Read the ledger into a fresh snapshot. Never panics: an unreachable ledger
    /// yields an empty snapshot carrying the error for the UI to surface.
    pub fn load(project: &wire::Project) -> Self {
        let ledger_path = PathBuf::from(project.ledger());
        let ledger_path = ledger_path.as_path();
        match Self::try_load(ledger_path, &project.socket()) {
            Ok(f) => f,
            Err(e) => Fleet {
                ledger_path: ledger_path.to_path_buf(),
                load_error: Some(e.to_string()),
                cogs: vec![],
                board: vec![],
                armada: vec![],
                convoys: vec![],
                escalations: vec![],
                orders: vec![],
                tick: None,
            },
        }
    }

    fn try_load(ledger_path: &Path, socket: &str) -> anyhow::Result<Self> {
        let db = Ledger::open_readonly(&ledger_path.to_string_lossy())?;
        let now = chrono::Utc::now();

        // fleetd's live liveness (keyed by session id) — best-effort; empty if the daemon
        // isn't up, in which case the roster shows report-age alone.
        let liveness = probe_liveness(socket);
        let mut cogs = Vec::new();
        for session in SessionRepo::live(&db)? {
            let report = ReportRepo::latest(&db, &session.id)?;
            let (state, last_action, stamp) = match report {
                Some(r) => (Some(r.state.as_str().to_string()), r.last_action, Some(r.reported_at)),
                None => (None, None, None),
            };
            // Age off the last report if the cog reports; else off session start (captain/lookout).
            let age_secs = age_secs(now, stamp.as_deref().unwrap_or(&session.created_at));
            let live = liveness.get(&session.id.0).cloned();
            cogs.push(CogRow { session, state, last_action, age_secs, liveness: live });
        }

        let mut armada = Vec::new();
        for ship in ShipRepo::armada(&db)? {
            let snapshot = ShipRepo::snapshot(&db, &ship.id)?;
            let sailor_state = last_sailor_state(&db, &ship)?;
            armada.push(ShipRow { ship, snapshot, sailor_state });
        }

        Ok(Fleet {
            ledger_path: ledger_path.to_path_buf(),
            load_error: None,
            cogs,
            board: TicketRepo::board(&db)?,
            armada,
            convoys: ConvoyRepo::active(&db)?,
            escalations: EscalationRepo::list_open(&db)?,
            orders: OrderRepo::queue(&db)?,
            tick: db.latest_tick()?,
        })
    }
}

/// A ship's most-recent sailor's last self-report (`sleeping` / `finishing` / …), or
/// `None` if no sailor has sailed it yet. Reads the NEWEST sailor session for the ship's
/// ticket — live or ended — so the state survives the cold-loop reap. Ships with no ticket
/// (allied MRs in the armada) never carry a sailor.
fn last_sailor_state(db: &Ledger, ship: &Ship) -> anyhow::Result<Option<String>> {
    let Some(ticket) = &ship.ticket else { return Ok(None) };
    let last = SessionRepo::for_ticket(db, ticket)?
        .into_iter()
        .filter(|s| s.kind == CrewKind::Sailor)
        .max_by_key(|s| s.id.0);
    match last {
        Some(s) => Ok(ReportRepo::latest(db, &s.id)?.map(|r| r.state.as_str().to_string())),
        None => Ok(None),
    }
}

/// Seconds between `now` and an RFC3339 timestamp; `None` if it doesn't parse.
fn age_secs(now: chrono::DateTime<chrono::Utc>, ts: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(ts)
        .ok()
        .map(|t| (now - t.with_timezone(&chrono::Utc)).num_seconds())
}

/// Overlay fleetd's live, tmux-probe-derived liveness (`Active/Quiet/Idle/Dead`) by
/// session id, read best-effort from the `/context` UDS endpoint. Empty when fleetd
/// isn't reachable — the whole dashboard still works off the ledger, just without the
/// real-time freshness. Liveness is deliberately NOT in the ledger (it'd be stale the
/// instant it's written); this is the one read that goes to the daemon, not the DB.
fn probe_liveness(socket: &str) -> std::collections::HashMap<i64, String> {
    let mut out = std::collections::HashMap::new();
    if let Ok((200, body)) = wire::get_json(socket, "/context") {
        if let Ok(serde_json::Value::Array(cogs)) = serde_json::from_str::<serde_json::Value>(&body) {
            for c in cogs {
                if let (Some(id), Some(live)) = (c["session"]["id"].as_i64(), c["liveness"].as_str()) {
                    out.insert(id, live.to_string());
                }
            }
        }
    }
    out
}
