//! Sync the ledger from the external surfaces (PLAN §3: upsert-all the armada/board).
//! The scheduler will call these on a slow cadence (minutes), separate from the ~5s
//! liveness poll. For now `fleetd sync` runs the armada sync on demand.

use anyhow::Result;
use domain::ports::{Forge, ShipRepo, TicketRepo, Tracker};
use domain::{Ship, ShipId, ShipSnapshot};
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
    Ok(count)
}

/// Fetch the armada from the forge and upsert each ship (+ its snapshot). A ship's
/// `ticket` FK is linked only if we already track that ticket; otherwise left `None`
/// (the tracker sync creates tickets; a later reconcile links any stragglers). This
/// keeps the forge-only path from tripping the `ships.ticket` foreign key.
pub fn sync_armada(ledger: &Ledger, forge: &dyn Forge, now: &str) -> Result<usize> {
    let ships = forge.armada()?;
    let count = ships.len();
    for fs in ships {
        let linked = matches!(&fs.ticket, Some(k) if TicketRepo::get(ledger, k)?.is_some());
        // Enrich OUR ships (linked to a tracked ticket) with pipeline/approvals/threads;
        // allies keep the cheap list data. Best-effort — a failed enrich falls back.
        let fs = if linked {
            match forge.snapshot(fs.iid) {
                Ok(enriched) => enriched,
                Err(e) => {
                    eprintln!("fleetd: enrich iid {} failed: {e}", fs.iid);
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
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::*;

    struct FakeForge(Vec<ForgeShip>);
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
            let mut fs = self.0.iter().find(|s| s.iid == iid).cloned()
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
        fn set_draft(&self, _: i64, _: bool) -> Result<()> {
            Ok(())
        }
        fn open_mr(&self, _: &domain::MrDraft) -> Result<String> {
            Ok("https://x/1".into())
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

        let forge = FakeForge(vec![fs(1, Some("PROJ-1")), fs(2, Some("PROJ-999")), fs(3, None)]);
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

    struct FakeTracker(Vec<TrackerTicket>);
    impl Tracker for FakeTracker {
        fn preflight(&self) -> Result<String> {
            Ok("fake".into())
        }
        fn board(&self) -> Result<Vec<TrackerTicket>> {
            Ok(self.0.clone())
        }
        fn get(&self, _: &TicketKey) -> Result<TrackerTicket> {
            unimplemented!()
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
        ]);
        let n = sync_board(&db, &tracker, "now").unwrap();
        assert_eq!(n, 2);
        assert_eq!(TicketRepo::board(&db).unwrap().len(), 2);
        assert_eq!(
            TicketRepo::get(&db, &TicketKey("PROJ-1".into())).unwrap().unwrap().status,
            Some(TicketStatus::InProgress)
        );
    }

    #[test]
    fn board_then_armada_links_ship_tickets() {
        let db = Ledger::in_memory().unwrap();
        // board first → PROJ-1 exists
        sync_board(&db, &FakeTracker(vec![tt("PROJ-1", TicketStatus::InProgress)]), "now").unwrap();
        // then armada → the ship for PROJ-1 links its FK
        sync_armada(&db, &FakeForge(vec![fs(1, Some("PROJ-1"))]), "now").unwrap();
        assert_eq!(
            ShipRepo::get_by_iid(&db, 1).unwrap().unwrap().ticket,
            Some(TicketKey("PROJ-1".into()))
        );
    }
}
