//! `fleetc crossings` — the facts a lookout's crossing check starts from: the MR under review, and
//! each of our open tickets beside it, with what the two MRs share. Gathered, never judged: the
//! ledger says which tickets are ours and open, the forge which paths each MR changes (one query
//! for all of them), and the tracker each ticket's text (one search). Which of them actually clash
//! is the lookout's call (and its spyglass's).

use crate::reconcile;
use anyhow::{anyhow, Result};
use domain::ports::{ConvoyRepo, Forge, ShipRepo, TicketRepo, Tracker};
use domain::{Crossing, Crossings, TicketStatus};
use ledger::Ledger;
use std::collections::BTreeSet;

/// The crossings of MR `iid`: our open tickets (in an active convoy, or carried by one of our open
/// MRs), its own ticket aside. `brief` leaves the tickets' text out (and the tracker unread).
pub fn crossings(ledger: &Ledger, tracker: &dyn Tracker, forge: &dyn Forge, iid: i64, brief: bool) -> Result<Crossings> {
    let armada = ShipRepo::armada(ledger)?;
    let open = |s: &&domain::Ship| s.ended_at.is_none();
    let ship = armada.iter().filter(open).find(|s| s.iid == iid).ok_or_else(|| anyhow!("!{iid}: not in the armada (synced yet?)"))?;
    let title = ShipRepo::snapshot(ledger, &ship.id)?.map(|s| s.title).unwrap_or_default();

    let active: Vec<_> = ConvoyRepo::active(ledger)?.into_iter().map(|c| c.key).collect();
    let mut ours = Vec::new();
    for t in TicketRepo::board(ledger)? {
        if matches!(t.status, Some(TicketStatus::Done | TicketStatus::Cancelled)) || ship.ticket.as_ref() == Some(&t.key) {
            continue;
        }
        let mr = armada.iter().filter(open).find(|s| s.ticket.as_ref() == Some(&t.key));
        let carried = match mr {
            Some(s) => reconcile::is_ours(ledger, s)?,
            None => false,
        };
        if t.convoy.as_ref().is_some_and(|c| active.contains(c)) || carried {
            ours.push((t, mr.map(|s| (s.iid, s.source.clone()))));
        }
    }

    let mut iids: Vec<i64> = ours.iter().filter_map(|(_, mr)| mr.as_ref().map(|m| m.0)).collect();
    iids.push(iid);
    let mut changed = forge.changed_paths(&iids)?;
    let paths = changed.iter().position(|(i, _)| *i == iid).map(|p| changed.remove(p).1).unwrap_or_default();

    let texts = if brief { vec![] } else { tracker.read_many(&ours.iter().map(|(t, _)| t.key.clone()).collect::<Vec<_>>())? };
    let tickets = ours
        .into_iter()
        .map(|(t, mr)| {
            let theirs = mr.as_ref().and_then(|(i, _)| changed.iter().find(|(c, _)| c == i)).map(|(_, p)| p.as_slice()).unwrap_or(&[]);
            let (shared_paths, shared_folders) = overlap(&paths, theirs);
            Crossing {
                text: (!brief).then(|| texts.iter().find(|d| d.ticket.key == t.key).map(|d| d.description.clone()).unwrap_or_default()),
                key: t.key,
                title: t.title.unwrap_or_default(),
                status: t.status,
                convoy: t.convoy,
                iid: mr.as_ref().map(|m| m.0),
                branch: mr.map(|m| m.1),
                shared_paths,
                shared_folders,
            }
        })
        .collect();
    Ok(Crossings { iid, title, paths, tickets })
}

/// What two MRs share: the paths both change, then the folders both change files in that those
/// paths don't already account for.
fn overlap(ours: &[String], theirs: &[String]) -> (Vec<String>, Vec<String>) {
    let theirs_set: BTreeSet<&String> = theirs.iter().collect();
    let shared: Vec<String> = ours.iter().filter(|p| theirs_set.contains(p)).cloned().collect();
    let in_shared: BTreeSet<String> = shared.iter().filter_map(|p| folder(p)).collect();
    let a: BTreeSet<String> = ours.iter().filter_map(|p| folder(p)).collect();
    let b: BTreeSet<String> = theirs.iter().filter_map(|p| folder(p)).collect();
    let folders = a.intersection(&b).filter(|f| !in_shared.contains(*f)).cloned().collect();
    (shared, folders)
}

/// A file's folder, to three levels (`crates/adapters/actor_store/src/x.rs` → its crate,
/// `crates/adapters/actor_store`); `None` for a file at the root, which every MR shares.
fn folder(path: &str) -> Option<String> {
    let dirs: Vec<&str> = path.split('/').collect();
    let dirs = &dirs[..dirs.len() - 1];
    (!dirs.is_empty()).then(|| dirs[..dirs.len().min(3)].join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|p| p.to_string()).collect()
    }

    #[test]
    fn two_mrs_share_their_common_paths_then_the_folders_they_touch_apart() {
        let ours = s(&["crates/adapters/actor_store/src/ddb.rs", "crates/domain/src/auth/scope.rs", "docs/a.md", "Cargo.lock"]);
        let theirs = s(&["crates/adapters/actor_store/src/reader.rs", "crates/domain/src/auth/scope.rs", "Cargo.lock"]);
        let (paths, folders) = overlap(&ours, &theirs);
        assert_eq!(paths, s(&["crates/domain/src/auth/scope.rs", "Cargo.lock"]));
        assert_eq!(folders, s(&["crates/adapters/actor_store"]), "the shared path's folder isn't repeated; the root never counts");
        assert_eq!(overlap(&ours, &[]), (vec![], vec![]));
    }

    #[test]
    fn a_files_folder_is_its_directory_to_three_levels() {
        assert_eq!(folder("crates/adapters/actor_store/src/lib.rs").as_deref(), Some("crates/adapters/actor_store"));
        assert_eq!(folder("docs/runbooks/x.md").as_deref(), Some("docs/runbooks"));
        assert_eq!(folder("README.md"), None);
    }
}
