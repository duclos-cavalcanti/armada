//! The fleet view model — a read-only snapshot of the ledger for the dashboard.
//!
//! Reloaded every refresh tick. The ledger is the single source of truth; this is
//! just a frozen read built from the typed repos, so the dashboard mirrors no schema
//! (that was the bash kit's `types.rs`). Opens the ledger READ-ONLY — a concurrent
//! reader beside fleetd's single writer (SQLite WAL), exactly like `fleetc plan`.
//!
//! The Fleet tab reads the line top-down: every dispatched order (the tickets the fleet is
//! working, each with its phase) → the live builders (worker / hand) → the ships being landed
//! (sailor). A ticket in phase 2 or 3 sits in exactly one of those two — the partition is here.
//!
//! Liveness note: the `Active/Quiet/Dead` freshness classification isn't in the
//! ledger (fleetd derives it in-memory from tmux probes). The proxy here is report
//! AGE — `now − reported_at` — which catches a stalled cog without re-probing tmux.

use domain::ports::{
    ConvoyRepo, OrderRepo, ProposalRepo, ReportRepo, SessionRepo, ShipRepo, TicketRepo,
};
use domain::{
    Convoy, CrewKind, Event, Order, Proposal, Session, Ship, ShipSnapshot, Ticket, TicketKey, TicketStatus,
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

/// A ship paired with its latest snapshot (if the armada sync has populated one) and its sailor's
/// label (`sailor_label`). It's durable past the reap: the cold loop ends a swept sailor's session,
/// but its `reports` row survives, so a ship at rest still shows `sleeping` (= attended, idle).
pub struct ShipRow {
    pub ship: Ship,
    pub snapshot: Option<ShipSnapshot>,
    pub sailor: String,
}

/// What a ship's sailor is doing: `unmanned` (no sailor has boarded), its last report, `sailing`
/// (aboard, nothing reported yet), or `·` (it ended without a word — crashed or sunk).
pub fn sailor_label(sailor: Option<&CogRow>) -> String {
    match sailor {
        None => "unmanned".into(),
        Some(s) => match (&s.state, s.session.ended_at.is_none()) {
            (Some(state), _) => state.clone(),
            (None, true) => "sailing".into(),
            (None, false) => "·".into(),
        },
    }
}

/// How far a dispatched ticket has got.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Its builder is due — none at work yet, and any MR it has waits (a rework).
    Waiting,
    /// A worker / hand is building it.
    Implementation,
    /// Its ship is being landed.
    Merge,
}

impl Phase {
    pub fn as_str(self) -> &'static str {
        match self {
            Phase::Waiting => "waiting",
            Phase::Implementation => "implementation",
            Phase::Merge => "merge",
        }
    }
}

/// A dispatched order — a ticket the fleet is working — with its phase and what it waits on.
pub struct OrderRow {
    pub order: Order,
    pub phase: Phase,
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
    /// Every dispatched order, with its phase.
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
    /// Work cogs found mid-ticket, waiting on the operator to accept or drop it.
    pub proposals: Vec<Proposal>,
    /// Tickets anchored in an open convoy and never dispatched — a captain judged them not Ready, and
    /// they wait on the operator to fix and weigh them.
    pub held: Vec<Ticket>,
    /// The latest reconcile `TickRan` event — fleetd's last plan (applied/failed/planned).
    pub tick: Option<Event>,
    /// The tmux sessions up right now — whether `enter` on a cog has a pane to switch to.
    pub tmux_up: HashSet<String>,
    /// fleetd answers on its socket.
    pub fleetd_up: bool,
    /// The tail of fleetd's log, newest first.
    pub log: Vec<String>,
}

impl Fleet {
    /// Read the ledger into a fresh snapshot. Never panics: an unreachable ledger
    /// yields an empty snapshot carrying the error for the UI to surface.
    /// The daemon's side (is it up, its log) is read even when the ledger isn't — a fleetd
    /// that failed to boot says why in its log.
    pub fn load(project: &wire::Project) -> Self {
        let ledger_path = PathBuf::from(project.ledger());
        // fleetd's live liveness (keyed by session id) — `None` when the daemon isn't up, in
        // which case the panes show report-age alone.
        let liveness = probe_liveness(&project.socket());
        let fleetd_up = liveness.is_some();
        let mut fleet = Self::try_load(&ledger_path, liveness.unwrap_or_default()).unwrap_or_else(|e| Fleet {
            ledger_path,
            load_error: Some(e.to_string()),
            ..Default::default()
        });
        fleet.fleetd_up = fleetd_up;
        fleet.log = log_tail(&project.log());
        fleet
    }

    fn try_load(ledger_path: &Path, liveness: HashMap<i64, String>) -> anyhow::Result<Self> {
        let db = Ledger::open_readonly(&ledger_path.to_string_lossy())?;
        let now = chrono::Utc::now();
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
        // Back in the line for a builder (dispatched, or reworked): its MR waits, sailor-less.
        let mut due = HashSet::new();
        for o in &orders {
            if o.builder_due(&SessionRepo::for_ticket(&db, &o.ticket)?) {
                due.insert(&o.ticket);
            }
        }
        let ships = ShipRepo::armada(&db)?;
        let mut armada = Vec::new();
        let mut landing = Vec::new();
        for ship in &ships {
            let snapshot = ShipRepo::snapshot(&db, &ship.id)?;
            let sailor = match &ship.ticket {
                Some(t) if !due.contains(t) => last_sailor(&db, t)?.map(&cog).transpose()?,
                _ => None, // an ally, or a ticket waiting for its builder: no sailor this round
            };
            armada.push(ShipRow {
                ship: ship.clone(),
                snapshot: snapshot.clone(),
                sailor: sailor_label(sailor.as_ref()),
            });
            let Some(ticket) = &ship.ticket else { continue }; // an ally: Ships tab only
            if building.contains(ticket) || due.contains(ticket) {
                continue; // its builder is at work or due: it shows in phase 1 or 2
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
            let phase = if building.contains(&order.ticket) {
                Phase::Implementation
            } else if sailing.contains(&order.ticket) && !due.contains(&order.ticket) {
                Phase::Merge
            } else {
                Phase::Waiting
            };
            let ticket = tickets.get(order.ticket.0.as_str());
            dispatched.push(OrderRow {
                phase,
                status: ticket.and_then(|t| t.status),
                held: held_reasons.get(&order.ticket.0).cloned(),
                blockers: ticket.map(|t| open_blockers(t, &board)).unwrap_or_default(),
                age_secs: age_secs(now, &order.created_at),
                order,
            });
        }

        let convoys = ConvoyRepo::active(&db)?;
        let held = board
            .iter()
            .filter(|t| t.held && !ordered.contains(&t.key))
            .filter(|t| t.convoy.as_ref().is_some_and(|c| convoys.iter().any(|open| &open.key == c)))
            .cloned()
            .collect();

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
            convoys,
            held,
            // A ledger older than the proposals table has none — fleetd adds it on its next start.
            proposals: db.pending().unwrap_or_default(),
            tick: db.latest_tick()?,
            tmux_up: tmux_sessions(),
            ..Default::default()
        })
    }

    /// Is this cog's tmux session up (so `enter` lands on it)?
    pub fn is_up(&self, cog: &CogRow) -> bool {
        self.tmux_up.contains(&cog.session.tmux_session)
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

/// The names of the tmux sessions up right now (empty when tmux isn't running).
fn tmux_sessions() -> HashSet<String> {
    std::process::Command::new("tmux")
        .args(["list-sessions", "-F", "#S"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).lines().map(str::to_string).collect())
        .unwrap_or_default()
}

/// The last lines of fleetd's log, newest first — read off the file's last 64 KiB, so a long run's
/// log costs the same as a fresh one. Empty when there's no log yet.
fn log_tail(path: &Path) -> Vec<String> {
    use std::io::{Read, Seek, SeekFrom};
    const TAIL: u64 = 64 * 1024;
    let Ok(mut file) = std::fs::File::open(path) else { return vec![] };
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    let mut buf = Vec::new();
    if file.seek(SeekFrom::Start(len.saturating_sub(TAIL))).and_then(|_| file.read_to_end(&mut buf)).is_err() {
        return vec![];
    }
    let text = String::from_utf8_lossy(&buf);
    let mut lines: Vec<&str> = text.lines().collect();
    if len > TAIL && !lines.is_empty() {
        lines.remove(0); // the seek landed mid-line
    }
    lines.into_iter().rev().map(str::to_string).collect()
}

/// Overlay fleetd's live, tmux-probe-derived liveness (`Active/Quiet/Idle/Dead`) by
/// session id, read best-effort from the `/context` UDS endpoint. `None` when fleetd
/// isn't reachable — the whole dashboard still works off the ledger, just without the
/// real-time freshness. Liveness is deliberately NOT in the ledger (it'd be stale the
/// instant it's written); this is the one read that goes to the daemon, not the DB.
fn probe_liveness(socket: &str) -> Option<HashMap<i64, String>> {
    let Ok((200, body)) = wire::get_json(socket, "/context") else { return None };
    let mut out = HashMap::new();
    if let Ok(serde_json::Value::Array(cogs)) = serde_json::from_str::<serde_json::Value>(&body) {
        for c in cogs {
            if let (Some(id), Some(live)) = (c["session"]["id"].as_i64(), c["liveness"].as_str()) {
                out.insert(id, live.to_string());
            }
        }
    }
    Some(out)
}
