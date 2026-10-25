//! fleetd — the fleet daemon (PLAN §2.1).
//!
//! This increment is the skeleton's **data path**: open the ledger, probe liveness
//! via the tmux `SessionHost`, and compute the `context` (every live session + its
//! derived liveness). The ~5s background poll loop and the UDS `/context` endpoint
//! are the next increment — this proves the read path end-to-end first.

mod collector;
mod config;
mod forge;
mod git;
mod otlp;
mod reconcile;
mod scheduler;
mod server;
mod settings;
mod sync;
mod tmux;
mod tracker;

use anyhow::Result;
use domain::ports::{SessionHost, SessionRepo};
use domain::{Liveness, LivenessThresholds, Session};
use ledger::Ledger;
use serde::Serialize;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use tmux::TmuxHost;

/// A context row: a live session + its liveness, DERIVED from a fresh host probe
/// (never stored — the whole point of the #49/#51 discipline).
#[derive(Clone, Serialize)]
pub struct Cog {
    pub session: Session,
    pub liveness: Liveness,
}

/// The latest context, memoized by the poll and served by the UDS endpoint.
pub type Snapshot = Arc<Mutex<Vec<Cog>>>;

fn now_epoch() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Compute the live context: every non-ended session + its liveness, each derived
/// from a fresh probe of the host (tmux). This is the cheap read the foreman's
/// `context` surface needs — the per-query fan-out that the ~5s poll will later
/// memoize in memory.
pub fn context(
    ledger: &Ledger,
    host: &dyn SessionHost,
    thresholds: &LivenessThresholds,
) -> Result<Vec<Cog>> {
    let now = now_epoch();
    let mut cogs = Vec::new();
    for session in ledger.live()? {
        let alive = host.alive(&session.tmux_session)?;
        let age = host.last_activity(&session.tmux_session)?.map(|t| now - t);
        let liveness = Liveness::classify(alive, age, thresholds);
        cogs.push(Cog { session, liveness });
    }
    Ok(cogs)
}

/// An RFC3339 UTC timestamp (the ledger's `created_at`/`synced_at` convention).
fn now_rfc3339() -> String {
    use time::format_description::well_known::Rfc3339;
    time::OffsetDateTime::now_utc().format(&Rfc3339).unwrap_or_default()
}

fn main() -> Result<()> {
    match std::env::args().nth(1).as_deref() {
        Some("sync") => run_sync(),
        _ => run_daemon(),
    }
}

/// One-shot: pull the armada from the forge into the ledger. The scheduler will run
/// this on a slow cadence later; `fleetd sync` lets an operator trigger it now.
fn run_sync() -> Result<()> {
    let cfg = config::Config::load()?;
    let ledger = Ledger::open(&cfg.ledger)?;
    let now = now_rfc3339();
    // Board first so ships can link their ticket FK.
    let tracker = tracker::JiraTracker::new(&cfg.tracker.project, cfg.tracker.claim_label.clone());
    let tickets = sync::sync_board(&ledger, &tracker, &now)?;
    let ships = sync::sync_armada(&ledger, &forge::GlabForge::new(&cfg.forge.project, cfg.ticket_prefix()), &now)?;
    println!("fleetd sync: {tickets} tickets + {ships} ships synced to the ledger");
    Ok(())
}

fn run_daemon() -> Result<()> {
    let cfg = config::Config::load()?;
    let ledger = Ledger::open(&cfg.ledger)?;
    let host = TmuxHost::new();

    // Seed the snapshot once so /context isn't empty before the first poll.
    let snapshot: Snapshot = Arc::new(Mutex::new(context(&ledger, &host, &cfg.thresholds())?));

    // Built-in OTLP collector: open telemetry.db (separate from the ledger) + serve the OTLP/HTTP
    // receiver on its own thread, so cog + one-off claude spend lands in telemetry.db.
    let telemetry = Arc::new(Mutex::new(ledger::telemetry::TelemetryStore::open(&cfg.telemetry_db)?));
    let otel_addr = cfg.otel_addr.clone();
    std::thread::spawn(move || {
        if let Err(e) = collector::serve(&otel_addr, telemetry) {
            eprintln!("fleetd: otlp collector exited: {e}");
        }
    });

    // The scheduler owns the ledger (single writer), paces the tickers, and drains
    // client write Commands off this channel; the server is the only sender.
    let (cmd_tx, cmd_rx) = std::sync::mpsc::channel();
    let socket = cfg.socket.clone();
    let sched_snapshot = snapshot.clone();

    // Online banner — the one line that says "up, and here's what I'll do", before the
    // scheduler thread consumes cfg + ledger. It carries the trust-critical facts: the
    // reconcile MODE (LIVE drives the line; dry-run only plans), how many running cogs the
    // tmux probe re-adopted (proof the restart resumed the fleet, not cold-started it), the
    // effective cadences, and the forge target. Read it as the go/no-go after a (re)start.
    let mode = if settings::dry_run(&cfg, &ledger) { "dry-run — planning only" } else { "LIVE — driving the line" };
    println!(
        "fleetd: ONLINE · {mode} · {} cogs adopted · poll {}s / sync {}s / reconcile {}s · forge {} · tracker {}",
        snapshot.lock().unwrap().len(),
        settings::poll_secs(&cfg, &ledger),
        settings::sync_secs(&cfg, &ledger),
        settings::reconcile_secs(&cfg, &ledger),
        cfg.forge.project,
        cfg.tracker.project,
    );
    std::thread::spawn(move || scheduler::run(ledger, host, sched_snapshot, cfg, cmd_rx));

    println!("fleetd: serving context on {socket}");
    server::serve(&socket, snapshot, cmd_tx) // blocks forever
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::ports::TicketRepo;
    use domain::*;
    use std::path::Path;

    /// A fake host lets us test context derivation without a real tmux.
    struct FakeHost {
        alive: bool,
        /// How many seconds ago the last activity was (None = no activity data).
        age_secs: Option<i64>,
    }
    impl SessionHost for FakeHost {
        fn spawn(&self, _: &str, _: &Path, _: &str) -> Result<()> {
            Ok(())
        }
        fn alive(&self, _: &str) -> Result<bool> {
            Ok(self.alive)
        }
        fn last_activity(&self, _: &str) -> Result<Option<i64>> {
            // Return an epoch instant `age_secs` in the past.
            Ok(self.age_secs.map(|a| now_epoch() - a))
        }
        fn kill(&self, _: &str) -> Result<()> {
            Ok(())
        }
        fn send(&self, _: &str, _: &str) -> Result<()> {
            Ok(())
        }
        fn list(&self) -> Result<Vec<String>> {
            Ok(vec![])
        }
    }

    fn ledger_with_live_worker() -> Ledger {
        let db = Ledger::in_memory().unwrap();
        TicketRepo::upsert(
            &db,
            &Ticket {
                key: TicketKey("PROJ-1".into()),
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
        SessionRepo::start(
            &db,
            &Session {
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
            },
        )
        .unwrap();
        db
    }

    #[test]
    fn fresh_session_is_active() {
        let db = ledger_with_live_worker();
        let cogs = context(&db, &FakeHost { alive: true, age_secs: Some(5) }, &LivenessThresholds::default()).unwrap();
        assert_eq!(cogs.len(), 1);
        assert_eq!(cogs[0].liveness, Liveness::Active);
    }

    #[test]
    fn host_gone_is_dead() {
        let db = ledger_with_live_worker();
        let cogs = context(&db, &FakeHost { alive: false, age_secs: None }, &LivenessThresholds::default()).unwrap();
        assert_eq!(cogs[0].liveness, Liveness::Dead);
    }

    #[test]
    fn stale_session_is_idle() {
        let db = ledger_with_live_worker();
        let cogs = context(&db, &FakeHost { alive: true, age_secs: Some(3600) }, &LivenessThresholds::default()).unwrap();
        assert_eq!(cogs[0].liveness, Liveness::Idle);
    }

    #[test]
    fn ended_sessions_are_not_in_context() {
        let db = ledger_with_live_worker();
        // End the only session → context is empty.
        let live = db.live().unwrap();
        db.end(&live[0].id, "t2").unwrap();
        let cogs = context(&db, &FakeHost { alive: true, age_secs: Some(5) }, &LivenessThresholds::default()).unwrap();
        assert!(cogs.is_empty());
    }
}
