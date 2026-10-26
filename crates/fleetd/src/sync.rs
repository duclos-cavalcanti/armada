//! Sync the ledger from the external surfaces (PLAN §3: upsert-all the armada/board).
//! The scheduler will call these on a slow cadence (minutes), separate from the ~5s
//! liveness poll. For now `fleetd sync` runs the armada sync on demand.

use anyhow::Result;
use domain::ports::{ConvoyRepo, Forge, OrderRepo, SessionRepo, ShipRepo, TicketRepo, Tracker};
use domain::{Ship, ShipId, ShipSnapshot, ShipState, TicketKey};
use ledger::Ledger;

/// Fetch the board from the tracker and upsert each ticket,
/// preserving our own `held`/`created_at`. Run this BEFORE `sync_armada` so ships
/// can link their `ticket` FK.
pub fn sync_board(ledger: &Ledger, tracker: &dyn Tracker, now: &str) -> Result<usize> {
    let tickets = tracker.board()?;
    let count = tickets.len();
    for t in &tickets {
        TicketRepo::sync(ledger, t, now)?;
    }
    // Off the board (no sprint, or another team's), the fleet still tracks what it works (an
    // order), what a convoy took on from another epic, and what those wait on: refresh them too, or
    // their status and `is blocked by` edges go stale — a sailor would boot blind to its blocker,
    // and the gate would never lift. A blocker syncs before its dependent, so the edge has its FK.
    let mut done: std::collections::HashSet<TicketKey> = tickets.iter().map(|t| t.key.clone()).collect();
    let tracked = OrderRepo::queue(ledger)?.into_iter().map(|o| o.ticket).chain(ledger.added_members()?);
    for key in tracked {
        if !done.insert(key.clone()) {
            continue;
        }
        let refreshed = tracker.get(&key).and_then(|t| {
            for b in t.blocked_by.iter().filter(|b| !done.contains(*b)).cloned().collect::<Vec<_>>() {
                done.insert(b.clone());
                if let Err(e) = tracker.get(&b).and_then(|bt| TicketRepo::sync(ledger, &bt, now)) {
                    tracing::warn!("sync: blocker {} of {} not refreshed: {e:#}", b.0, key.0);
                }
            }
            TicketRepo::sync(ledger, &t, now)
        });
        if let Err(e) = refreshed {
            tracing::warn!("sync: {} (off the board) not refreshed: {e:#}", key.0);
        }
    }
    Ok(count)
}

/// Fetch the armada from the forge and upsert each ship (+ its snapshot). A ship's
/// `ticket` FK is linked only if we already track that ticket; otherwise left `None`
/// (the tracker sync creates tickets; a later reconcile links any stragglers). This
/// keeps the forge-only path from tripping the `ships.ticket` foreign key.
pub fn sync_armada(ledger: &Ledger, forge: &dyn Forge, now: &str) -> Result<usize> {
    let ships = forge.armada()?;
    let count = ships.len();
    let open: std::collections::HashSet<i64> = ships.iter().map(|s| s.iid).collect();
    for fs in ships {
        let linked = matches!(&fs.ticket, Some(k) if TicketRepo::get(ledger, k)?.is_some());
        // Enrich OUR ships (linked to a tracked ticket) with pipeline/approvals/threads;
        // allies keep the cheap list data. Best-effort — a failed enrich falls back.
        let fs = if linked {
            match forge.snapshot(fs.iid) {
                Ok(enriched) => enriched,
                Err(e) => {
                    tracing::warn!("enrich !{} failed: {e:#}", fs.iid);
                    fs
                }
            }
        } else {
            fs
        };
        let ticket = if linked { fs.ticket.clone() } else { None };
        let id = ShipRepo::upsert(
            ledger,
            &Ship {
                id: ShipId(0), // ignored; assigned/looked-up by iid
                ticket,
                iid: fs.iid,
                url: fs.url,
                target: fs.target,
                source: fs.source,
                pilot: fs.pilot,
                created_at: now.to_string(),
                ended_at: None,
                swept: None,
                reviewed: None,
            },
        )?;
        ledger.sync_snapshot(&ShipSnapshot {
            ship: id,
            title: fs.title,
            state: fs.state,
            head_sha: fs.head_sha,
            has_conflicts: fs.has_conflicts,
            open_threads: fs.open_threads,
            approvals: fs.approvals,
            armed: fs.armed,
            on_merge_train: fs.on_merge_train,
            pipeline: fs.pipeline,
            merge_status: fs.merge_status,
            synced_at: now.to_string(),
        })?;
    }
    retire_departed(ledger, forge, &open, now)?;
    Ok(count)
}

/// Ships that left the forge's open list merged or closed — the list never says which. An ally's
/// just ends. Ours is re-read once for its final state, then ends — unless it merged and its ticket
/// still has an order or a live cog: reconcile reaps that one (Jira → Done, cleanup) and ends it.
/// Best-effort per ship: a failed read is retried next sync.
fn retire_departed(ledger: &Ledger, forge: &dyn Forge, open: &std::collections::HashSet<i64>, now: &str) -> Result<()> {
    for ship in ledger.armada()?.into_iter().filter(|s| !open.contains(&s.iid)) {
        let Some(ticket) = &ship.ticket else {
            ShipRepo::retire(ledger, &ship.id, now)?;
            continue;
        };
        let fs = match forge.snapshot(ship.iid) {
            Ok(fs) => fs,
            Err(e) => {
                tracing::warn!("sync: departed !{} not re-read: {e:#}", ship.iid);
                continue;
            }
        };
        let merged = fs.state == ShipState::Merged;
        ledger.sync_snapshot(&ShipSnapshot {
            ship: ship.id.clone(),
            title: fs.title,
            state: fs.state,
            head_sha: fs.head_sha,
            has_conflicts: fs.has_conflicts,
            open_threads: fs.open_threads,
            approvals: fs.approvals,
            armed: fs.armed,
            on_merge_train: fs.on_merge_train,
            pipeline: fs.pipeline,
            merge_status: fs.merge_status,
            synced_at: now.to_string(),
        })?;
        let to_reap = merged
            && (OrderRepo::get(ledger, ticket)?.is_some()
                || SessionRepo::for_ticket(ledger, ticket)?.iter().any(|s| s.ended_at.is_none()));
        if !to_reap {
            ShipRepo::retire(ledger, &ship.id, now)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::*;

    /// The open MRs (`.0`) plus ones only `snapshot` reaches (`.1`): merged or closed since.
    struct FakeForge(Vec<ForgeShip>, Vec<ForgeShip>);
    impl Forge for FakeForge {
        fn preflight(&self) -> Result<String> {
            Ok("fake".into())
        }
        fn stacks(&self) -> bool {
            true
        }
        fn armada(&self) -> Result<Vec<ForgeShip>> {
            Ok(self.0.clone())
        }
        fn snapshot(&self, iid: i64) -> Result<ForgeShip> {
            // Simulate enrichment: the detail fills approvals + pipeline.
            let mut fs = self.0.iter().chain(&self.1).find(|s| s.iid == iid).cloned()
                .ok_or_else(|| anyhow::anyhow!("no mr {iid}"))?;
            fs.approvals = Approvals { current: 1, required: 1 };
            fs.pipeline = Some(PipelineStatus::Success);
            Ok(fs)
        }
        fn review_state(&self, _: i64) -> Result<ReviewState> {
            unimplemented!()
        }
        fn rebase(&self, _: i64) -> Result<()> {
            Ok(())
        }
        fn block_on(&self, _: i64, _: i64) -> Result<()> {
            Ok(())
        }
        fn arm_merge(&self, _: i64) -> Result<()> {
            Ok(())
        }
        fn disarm(&self, _: i64) -> Result<()> {
            Ok(())
        }
        fn close_mr(&self, _: i64) -> Result<()> {
            Ok(())
        }
        fn set_draft(&self, _: i64, _: bool) -> Result<()> {
            Ok(())
        }
        fn open_mr(&self, _: &domain::MrDraft) -> Result<String> {
            Ok("https://x/1".into())
        }
        fn ci(&self, _: i64) -> Result<domain::CiReport> {
            unimplemented!()
        }
        fn retry_ci(&self, _: i64) -> Result<()> {
            Ok(())
        }
        fn reply(&self, _: i64, _: &str, _: &str, _: bool) -> Result<()> {
            Ok(())
        }
        fn fetch_ref(&self, iid: i64) -> String {
            format!("pull/{iid}/head")
        }
        fn post_review(&self, _: i64, _: &str, _: bool) -> Result<()> {
            Ok(())
        }
        fn request_review(&self, _: &domain::TicketKey) -> Result<Vec<String>> {
            Ok(vec![])
        }
    }

    fn fs(iid: i64, ticket: Option<&str>) -> ForgeShip {
        ForgeShip {
            iid,
            url: "u".into(),
            target: "master".into(),
            source: String::new(),
            pilot: "p".into(),
            ticket: ticket.map(|t| TicketKey(t.into())),
            title: "T".into(),
            state: ShipState::Open,
            head_sha: "sha".into(),
            has_conflicts: false,
            open_threads: 0,
            approvals: Approvals { current: 0, required: 0 },
            armed: false,
            on_merge_train: false,
            pipeline: None,
            merge_status: None,
        }
    }

    #[test]
    fn syncs_ships_and_links_only_known_tickets() {
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

        let forge = FakeForge(vec![fs(1, Some("PROJ-1")), fs(2, Some("PROJ-999")), fs(3, None)], vec![]);
        let n = sync_armada(&db, &forge, "now").unwrap();

        assert_eq!(n, 3);
        assert_eq!(ShipRepo::armada(&db).unwrap().len(), 3);
        // PROJ-1 is tracked → linked; PROJ-999 unknown → None; iid 3 had no ticket.
        assert_eq!(
            ShipRepo::get_by_iid(&db, 1).unwrap().unwrap().ticket,
            Some(TicketKey("PROJ-1".into()))
        );
        assert_eq!(ShipRepo::get_by_iid(&db, 2).unwrap().unwrap().ticket, None);
        // and a snapshot was written.
        let id = ShipRepo::get_by_iid(&db, 1).unwrap().unwrap().id;
        assert!(db.snapshot(&id).unwrap().is_some());

        // OUR ship (PROJ-1, tracked) is enriched; the unknown/allied ships are not.
        assert_eq!(db.snapshot(&id).unwrap().unwrap().approvals, Approvals { current: 1, required: 1 });
        let ally = ShipRepo::get_by_iid(&db, 2).unwrap().unwrap().id; // PROJ-999, untracked
        assert_eq!(db.snapshot(&ally).unwrap().unwrap().approvals, Approvals { current: 0, required: 0 });
    }

    /// The sprint board (`.0`) plus tickets only `get` reaches (`.1`): off the board, in no sprint.
    struct FakeTracker(Vec<TrackerTicket>, Vec<TrackerTicket>);
    impl Tracker for FakeTracker {
        fn preflight(&self) -> Result<String> {
            Ok("fake".into())
        }
        fn board(&self) -> Result<Vec<TrackerTicket>> {
            Ok(self.0.clone())
        }
        fn get(&self, key: &TicketKey) -> Result<TrackerTicket> {
            self.0.iter().chain(&self.1).find(|t| &t.key == key).cloned().ok_or_else(|| anyhow::anyhow!("no {}", key.0))
        }
        fn transition(&self, _: &TicketKey, _: TicketStatus) -> Result<()> {
            Ok(())
        }
        fn assigned_to_peer(&self, _: &TicketKey) -> Result<bool> {
            Ok(false)
        }
        fn claim(&self, _: &TicketKey) -> Result<()> {
            Ok(())
        }
        fn parent_of(&self, _: &TicketKey) -> Result<Option<TicketKey>> {
            Ok(None)
        }
        fn subtasks(&self, _: &TicketKey) -> Result<Vec<TicketStatus>> {
            Ok(vec![])
        }
        fn read(&self, _: &TicketKey) -> Result<domain::TicketDetail> {
            unimplemented!()
        }
        fn create(&self, _: &domain::TicketDraft) -> Result<TicketKey> {
            unimplemented!()
        }
        fn block(&self, _: &TicketKey, _: &TicketKey) -> Result<()> {
            Ok(())
        }
        fn comment(&self, _: &TicketKey, _: &str) -> Result<()> {
            Ok(())
        }
    }

    fn tt(key: &str, status: TicketStatus) -> TrackerTicket {
        TrackerTicket {
            key: TicketKey(key.into()),
            title: "T".into(),
            status,
            assignee: Some("me".into()),
            convoy: None,
            blocked_by: vec![],
        }
    }

    #[test]
    fn sync_board_upserts_the_sprint() {
        let db = Ledger::in_memory().unwrap();
        let tracker = FakeTracker(vec![
            tt("PROJ-1", TicketStatus::InProgress),
            tt("PROJ-2", TicketStatus::Todo),
        ], vec![]);
        let n = sync_board(&db, &tracker, "now").unwrap();
        assert_eq!(n, 2);
        assert_eq!(TicketRepo::board(&db).unwrap().len(), 2);
        assert_eq!(
            TicketRepo::get(&db, &TicketKey("PROJ-1".into())).unwrap().unwrap().status,
            Some(TicketStatus::InProgress)
        );
    }

    #[test]
    fn a_worked_ticket_off_the_board_is_refreshed_with_its_blocker() {
        // PROJ-3 fell out of every sprint, but the fleet still works it (an order); since its last
        // sync it moved on and gained a blocker, PROJ-4 — also off the board.
        let db = Ledger::in_memory().unwrap();
        let stale = tt("PROJ-3", TicketStatus::Todo);
        sync_board(&db, &FakeTracker(vec![stale], vec![]), "then").unwrap();
        db.dispatch(&Order {
            ticket: TicketKey("PROJ-3".into()),
            auto_impl: true,
            auto_merge: true,
            fork_from: "origin/main".into(),
            ac_summary: "do it".into(),
            park_at: None,
            worktree_path: None,
            created_at: "then".into(),
        })
        .unwrap();
        let mut moved = tt("PROJ-3", TicketStatus::InQa);
        moved.blocked_by = vec![TicketKey("PROJ-4".into())];
        let tracker = FakeTracker(vec![tt("PROJ-1", TicketStatus::Todo)], vec![moved, tt("PROJ-4", TicketStatus::InQa)]);

        sync_board(&db, &tracker, "now").unwrap();
        let t = TicketRepo::get(&db, &TicketKey("PROJ-3".into())).unwrap().unwrap();
        assert_eq!(t.status, Some(TicketStatus::InQa), "refreshed off the board");
        assert_eq!(t.blocked_by, vec![TicketKey("PROJ-4".into())], "its blocker synced first, so the edge holds");
        assert_eq!(TicketRepo::get(&db, &TicketKey("PROJ-4".into())).unwrap().unwrap().status, Some(TicketStatus::InQa));
    }

    #[test]
    fn a_ship_that_left_the_open_list_ends_unless_its_merge_waits_on_a_reap() {
        let db = Ledger::in_memory().unwrap();
        let board = vec![tt("PROJ-1", TicketStatus::InQa), tt("PROJ-2", TicketStatus::InQa)];
        sync_board(&db, &FakeTracker(board, vec![]), "now").unwrap();
        sync_armada(&db, &FakeForge(vec![fs(1, Some("PROJ-1")), fs(2, Some("PROJ-2")), fs(3, None)], vec![]), "now").unwrap();
        db.dispatch(&Order {
            ticket: TicketKey("PROJ-2".into()),
            auto_impl: true,
            auto_merge: true,
            fork_from: "origin/main".into(),
            ac_summary: "do it".into(),
            park_at: None,
            worktree_path: None,
            created_at: "then".into(),
        })
        .unwrap();
        // All three left the open list: !1 closed, !2 merged (its order still open), !3 an ally.
        let mut closed = fs(1, Some("PROJ-1"));
        closed.state = ShipState::Closed;
        let mut merged = fs(2, Some("PROJ-2"));
        merged.state = ShipState::Merged;
        sync_armada(&db, &FakeForge(vec![], vec![closed, merged]), "later").unwrap();

        let open: Vec<i64> = db.armada().unwrap().iter().map(|s| s.iid).collect();
        assert_eq!(open, vec![2], "the closed one and the ally end; the merge waits for its reap");
        let s2 = ShipRepo::get_by_iid(&db, 2).unwrap().unwrap();
        assert_eq!(ShipRepo::snapshot(&db, &s2.id).unwrap().unwrap().state, ShipState::Merged, "its final state, for reconcile");
    }

    #[test]
    fn board_then_armada_links_ship_tickets() {
        let db = Ledger::in_memory().unwrap();
        // board first → PROJ-1 exists
        sync_board(&db, &FakeTracker(vec![tt("PROJ-1", TicketStatus::InProgress)], vec![]), "now").unwrap();
        // then armada → the ship for PROJ-1 links its FK
        sync_armada(&db, &FakeForge(vec![fs(1, Some("PROJ-1"))], vec![]), "now").unwrap();
        assert_eq!(
            ShipRepo::get_by_iid(&db, 1).unwrap().unwrap().ticket,
            Some(TicketKey("PROJ-1".into()))
        );
    }
}
