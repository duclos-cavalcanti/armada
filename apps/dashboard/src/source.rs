//! The fleet view model — a read-only snapshot of the ledger for the dashboard.
//!
//! Reloaded every refresh tick. The ledger is the single source of truth; this is
//! just a frozen read built from the typed repos, so the dashboard mirrors no schema
//! (that was the bash kit's `types.rs`). Opens the ledger READ-ONLY — a concurrent
//! reader beside fleetd's single writer (SQLite WAL), exactly like `fleetc plan`.
//!
//! The Fleet tab reads the line top-down by how far each ticket has got: a dispatched
//! order waiting for its builder → a live builder (worker / hand) → a ship being landed
//! (sailor). Each ticket sits in exactly one of the three — the partition is here.
//!
//! Liveness note: the `Active/Quiet/Dead` freshness classification isn't in the
//! ledger (fleetd derives it in-memory from tmux probes). The proxy here is report
//! AGE — `now − reported_at` — which catches a stalled cog without re-probing tmux.

use domain::ports::{
    ConvoyRepo, EscalationRepo, OrderRepo, ReportRepo, SessionRepo, ShipRepo, TicketRepo,
};
use domain::{
    Convoy, CrewKind, Escalation, Event, Order, Session, Ship, ShipSnapshot, Ticket, TicketKey, TicketStatus,
};
use ledger::Ledger;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// One cog, projected for a pane: its session, its latest self-report (for the
/// reporting kinds — worker/sailor/hand; `None` for the report-less captain/lookout),
/// the age of that report in seconds, and fleetd's live tmux-probe liveness
/// (`Active/Quiet/Idle/Dead`) when the daemon is reachable and the session is live
/// (`None` otherwise → fall back to report-age).
#[derive(Clone)]
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

/// Phase 1 — a dispatched order with no builder at work and no ship yet: what it waits on.
pub struct OrderRow {
    pub order: Order,
    /// The ticket's status, `None` when the ticket isn't on the board.
    pub status: Option<TicketStatus>,
    /// `Some(reason)` when the ticket is held (`""` when no reason was given).
    pub held: Option<String>,
    pub blockers: Vec<String>,
    pub age_secs: Option<i64>,
}

/// Phase 3 — a ship the fleet took on (its ticket has an order, or a sailor has sailed it)
/// being landed, with no builder at work on it.
/// `sailor` is its NEWEST sailor session — live or ended — so its last state survives
/// the cold-loop reap (`live` tells which).
pub struct LandingRow {
    pub ship: Ship,
    pub snapshot: Option<ShipSnapshot>,
    pub sailor: Option<CogRow>,
    pub live: bool,
    /// What it waits on before it can merge: a stacked parent's ticket (its target is
    /// another ship's source branch) and the ticket's open blockers.
    pub blocked_on: Vec<String>,
}

/// The whole dashboard read — one atomic snapshot of the ledger. `load_error` is set
/// (and the rest left empty) when the ledger can't be opened — the normal cold-start
/// state before fleetd has created it.
#[derive(Default)]
pub struct Fleet {
    pub ledger_path: PathBuf,
    pub load_error: Option<String>,
    /// Phase 1: dispatched orders waiting for their builder.
    pub dispatched: Vec<OrderRow>,
    /// Phase 2: live builders (workers + hands).
    pub builders: Vec<CogRow>,
    /// Phase 3: our ships being landed.
    pub landing: Vec<LandingRow>,
    /// The ticketless cogs: captains (by convoy) + the lookout.
    pub crew: Vec<CogRow>,
    pub board: Vec<Ticket>,
    /// Why each held ticket is held (its key → the hold's reason, `""` when none was given).
    pub held_reasons: HashMap<String, String>,
    pub armada: Vec<ShipRow>,
    pub convoys: Vec<Convoy>,
    pub escalations: Vec<Escalation>,
    /// The latest reconcile `TickRan` event — fleetd's last plan (applied/failed/planned).
    pub tick: Option<Event>,
}

impl Fleet {
    /// Read the ledger into a fresh snapshot. Never panics: an unreachable ledger
    /// yields an empty snapshot carrying the error for the UI to surface.
    pub fn load(project: &wire::Project) -> Self {
        let ledger_path = PathBuf::from(project.ledger());
        Self::try_load(&ledger_path, &project.socket()).unwrap_or_else(|e| Fleet {
            ledger_path,
            load_error: Some(e.to_string()),
            ..Default::default()
        })
    }

    fn try_load(ledger_path: &Path, socket: &str) -> anyhow::Result<Self> {
        let db = Ledger::open_readonly(&ledger_path.to_string_lossy())?;
        let now = chrono::Utc::now();
        // fleetd's live liveness (keyed by session id) — best-effort; empty if the daemon
        // isn't up, in which case the panes show report-age alone.
        let liveness = probe_liveness(socket);
        let cog = |session: Session| -> anyhow::Result<CogRow> {
            let report = ReportRepo::latest(&db, &session.id)?;
            let (state, last_action, stamp) = match report {
                Some(r) => (Some(r.state.as_str().to_string()), r.last_action, Some(r.reported_at)),
                None => (None, None, None),
            };
            // Age off the last report if the cog reports; else off session start (captain/lookout).
            let age_secs = age_secs(now, stamp.as_deref().unwrap_or(&session.created_at));
            let liveness = liveness.get(&session.id.0).cloned();
            Ok(CogRow { session, state, last_action, age_secs, liveness })
        };

        let (mut builders, mut crew) = (Vec::new(), Vec::new());
        for session in SessionRepo::live(&db)? {
            match session.kind {
                CrewKind::Worker | CrewKind::Hand => builders.push(cog(session)?),
                CrewKind::Captain | CrewKind::Lookout => crew.push(cog(session)?),
                CrewKind::Sailor => {} // a sailor shows with its ship, in `landing`
            }
        }
        let building: HashSet<&TicketKey> = builders.iter().filter_map(|c| c.session.ticket.as_ref()).collect();

        let board = TicketRepo::board(&db)?;
        let tickets: HashMap<&str, &Ticket> = board.iter().map(|t| (t.key.0.as_str(), t)).collect();
        let mut held_reasons = HashMap::new();
        for t in board.iter().filter(|t| t.held) {
            held_reasons.insert(t.key.0.clone(), db.held_reason(&t.key)?.unwrap_or_default());
        }

        let orders = OrderRepo::queue(&db)?;
        let ordered: HashSet<&TicketKey> = orders.iter().map(|o| &o.ticket).collect();
        let ships = ShipRepo::armada(&db)?;
        let mut armada = Vec::new();
        let mut landing = Vec::new();
        for ship in &ships {
            let snapshot = ShipRepo::snapshot(&db, &ship.id)?;
            let sailor = match &ship.ticket {
                Some(t) => last_sailor(&db, t)?.map(&cog).transpose()?,
                None => None,
            };
            armada.push(ShipRow {
                ship: ship.clone(),
                snapshot: snapshot.clone(),
                sailor_state: sailor.as_ref().and_then(|s| s.state.clone()),
            });
            let Some(ticket) = &ship.ticket else { continue }; // an ally: Ships tab only
            if building.contains(ticket) {
                continue; // its builder is still at work: it shows in phase 2
            }
            if sailor.is_none() && !ordered.contains(ticket) {
                continue; // a ticketed MR the fleet never took on (a teammate's): Ships tab only
            }
            let mut blocked_on: Vec<String> = ships
                .iter()
                .filter(|parent| parent.source == ship.target)
                .map(|parent| parent.ticket.as_ref().map_or_else(|| format!("!{}", parent.iid), |k| k.0.clone()))
                .collect();
            for b in tickets.get(ticket.0.as_str()).map(|t| open_blockers(t, &board)).unwrap_or_default() {
                if !blocked_on.contains(&b) {
                    blocked_on.push(b);
                }
            }
            let live = sailor.as_ref().is_some_and(|s| s.session.ended_at.is_none());
            landing.push(LandingRow { ship: ship.clone(), snapshot, sailor, live, blocked_on });
        }
        let sailing: HashSet<&TicketKey> = ships.iter().filter_map(|s| s.ticket.as_ref()).collect();

        let mut dispatched = Vec::new();
        for order in orders.iter().cloned() {
            if building.contains(&order.ticket) || sailing.contains(&order.ticket) {
                continue;
            }
            let ticket = tickets.get(order.ticket.0.as_str());
            dispatched.push(OrderRow {
                status: ticket.and_then(|t| t.status),
                held: held_reasons.get(&order.ticket.0).cloned(),
                blockers: ticket.map(|t| open_blockers(t, &board)).unwrap_or_default(),
                age_secs: age_secs(now, &order.created_at),
                order,
            });
        }

        Ok(Fleet {
            ledger_path: ledger_path.to_path_buf(),
            load_error: None,
            dispatched,
            builders,
            landing,
            crew,
            board,
            held_reasons,
            armada,
            convoys: ConvoyRepo::active(&db)?,
            escalations: EscalationRepo::list_open(&db)?,
            tick: db.latest_tick()?,
        })
    }

    /// A ticket's open ship, if it has one.
    pub fn ship_of(&self, ticket: Option<&TicketKey>) -> Option<&ShipRow> {
        let ticket = ticket?;
        self.armada.iter().find(|r| r.ship.ticket.as_ref() == Some(ticket))
    }

    /// The live captain commanding a convoy, if one is.
    pub fn captain_of(&self, convoy: &domain::ConvoyKey) -> Option<&CogRow> {
        self.crew.iter().find(|c| c.session.convoy.as_ref() == Some(convoy))
    }

    /// The live lookout, if one is reviewing.
    pub fn lookout(&self) -> Option<&CogRow> {
        self.crew.iter().find(|c| c.session.kind == CrewKind::Lookout)
    }
}

/// A ticket's blockers that haven't landed: not Done/Cancelled on the board (a blocker
/// off the board counts as open — the board is all we know).
pub fn open_blockers(t: &Ticket, board: &[Ticket]) -> Vec<String> {
    t.blocked_by
        .iter()
        .filter(|b| {
            !board.iter().any(|o| {
                &o.key == *b && matches!(o.status, Some(TicketStatus::Done) | Some(TicketStatus::Cancelled))
            })
        })
        .map(|b| b.0.clone())
        .collect()
}

/// A ticket's NEWEST sailor session — live or ended — or `None` if no sailor has sailed it yet.
fn last_sailor(db: &Ledger, ticket: &TicketKey) -> anyhow::Result<Option<Session>> {
    Ok(SessionRepo::for_ticket(db, ticket)?
        .into_iter()
        .filter(|s| s.kind == CrewKind::Sailor)
        .max_by_key(|s| s.id.0))
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
fn probe_liveness(socket: &str) -> HashMap<i64, String> {
    let mut out = HashMap::new();
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
