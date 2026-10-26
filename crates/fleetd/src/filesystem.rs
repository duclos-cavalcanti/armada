//! The filesystem Tracker driver — tickets as markdown files in a folder, one `<KEY>.md` each. No
//! service, no CLI: the folder is the tracker. Each file opens with a YAML header the fleet reads
//! and writes (title, type, status, convoy, blocked_by, assignee, parent); the body below it is the
//! human-authored description, which fleetd never touches.
//!
//! ```markdown
//! ---
//! title: Add neg to the sandbox crate
//! type: story
//! status: todo
//! blocked_by: [ARMT-3]
//! ---
//! ## Outcome
//! …
//! ```
//!
//! The board is every ticket file. A claim assigns the ticket to the configured user. A file whose
//! name, key prefix, or header doesn't parse is skipped by the board (and named by preflight), so
//! one bad file never stalls the fleet.

use anyhow::{anyhow, bail, ensure, Context, Result};
use domain::ports::Tracker;
use domain::{ConvoyKey, TicketDetail, TicketDraft, TicketKey, TicketStatus, TrackerTicket};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub struct FsTracker {
    /// The folder holding the ticket files.
    dir: PathBuf,
    /// The ticket key prefix (`[tracker] project`) every file's key must carry.
    prefix: String,
    /// Who "me" is: a claim assigns this user; anyone else assigned is a peer.
    user: String,
}

/// The fleet-managed header of a ticket file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Header {
    title: String,
    /// The ticket's type (`story`, `bug`, `task`, `epic`, …) — a free word; fleet-berth derives the
    /// branch prefix from it.
    #[serde(rename = "type")]
    kind: String,
    status: TicketStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    convoy: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    blocked_by: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    assignee: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    parent: Option<String>,
}

/// A folder scan: `(key, ticket)` per good file, `(file, why)` per bad one.
type Scan = (Vec<(TicketKey, TicketFile)>, Vec<(PathBuf, anyhow::Error)>);

/// A parsed ticket file: its header + its untouched body.
struct TicketFile {
    header: Header,
    body: String,
}

impl FsTracker {
    pub fn new(dir: impl Into<PathBuf>, prefix: impl Into<String>, user: impl Into<String>) -> Self {
        Self { dir: dir.into(), prefix: prefix.into(), user: user.into() }
    }

    fn path(&self, key: &TicketKey) -> PathBuf {
        self.dir.join(format!("{}.md", key.0))
    }

    fn load(&self, key: &TicketKey) -> Result<TicketFile> {
        let path = self.path(key);
        let text = std::fs::read_to_string(&path).with_context(|| format!("ticket {}: {}", key.0, path.display()))?;
        parse(&text).with_context(|| format!("ticket {}", path.display()))
    }

    /// Rewrite a ticket's header (the body is kept byte for byte), atomically: a temp file renamed
    /// over the original, so a reader never sees half a ticket.
    fn write(&self, key: &TicketKey, t: &TicketFile) -> Result<()> {
        let path = self.path(key);
        let tmp = path.with_extension("md.tmp");
        std::fs::write(&tmp, render(t)?).with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, &path).with_context(|| format!("replacing {}", path.display()))
    }

    /// Read-modify-write one ticket's header.
    fn update(&self, key: &TicketKey, edit: impl FnOnce(&mut Header)) -> Result<()> {
        let mut t = self.load(key)?;
        let before = t.header.clone();
        edit(&mut t.header);
        if t.header != before {
            self.write(key, &t)?; // an unchanged header is a no-op, so edits are idempotent
        }
        Ok(())
    }

    /// Every ticket file in the folder: the good ones parsed, the rest with why they failed.
    fn scan(&self) -> Result<Scan> {
        let entries = std::fs::read_dir(&self.dir).with_context(|| format!("reading tickets in {}", self.dir.display()))?;
        let (mut good, mut bad) = (Vec::new(), Vec::new());
        let mut paths: Vec<PathBuf> = entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "md"))
            .collect();
        paths.sort();
        for path in paths {
            match self.key_of(&path).and_then(|key| Ok((key.clone(), self.load(&key)?))) {
                Ok(ticket) => good.push(ticket),
                Err(e) => bad.push((path, e)),
            }
        }
        Ok((good, bad))
    }

    /// The key a file name declares — it must be a key with this tracker's prefix.
    fn key_of(&self, path: &Path) -> Result<TicketKey> {
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        let key = TicketKey::parse(stem).map_err(|e| anyhow!("file name isn't a ticket key: {e}"))?;
        ensure!(
            key.0.rsplit_once('-').map(|(p, _)| p) == Some(self.prefix.as_str()),
            "{} isn't a {}-N key",
            key.0,
            self.prefix
        );
        Ok(key)
    }

    fn to_ticket(&self, key: TicketKey, t: &TicketFile) -> TrackerTicket {
        let keys = |v: &[String]| v.iter().filter_map(|k| TicketKey::parse(k).ok()).collect();
        TrackerTicket {
            key,
            title: t.header.title.clone(),
            status: t.header.status,
            assignee: t.header.assignee.clone(),
            convoy: t.header.convoy.as_deref().and_then(|c| ConvoyKey::parse(c).ok()),
            blocked_by: keys(&t.header.blocked_by),
        }
    }
}

/// Split a ticket file into its YAML header (between the opening `---` and the next `---` line)
/// and its body.
fn parse(text: &str) -> Result<TicketFile> {
    let rest = text.strip_prefix("---\n").ok_or_else(|| anyhow!("no YAML header (the file must open with ---)"))?;
    let (yaml, body) = match rest.find("\n---\n") {
        Some(i) => (&rest[..=i], &rest[i + 5..]),
        None => match rest.strip_suffix("\n---") {
            Some(yaml) => (yaml, ""),
            None => bail!("the YAML header isn't closed (no --- line after it)"),
        },
    };
    let header = serde_saphyr::from_str(yaml).map_err(|e| anyhow!("header: {e}"))?;
    Ok(TicketFile { header, body: body.to_string() })
}

fn render(t: &TicketFile) -> Result<String> {
    let yaml = serde_saphyr::to_string(&t.header).map_err(|e| anyhow!("header: {e}"))?;
    let newline = if yaml.ends_with('\n') { "" } else { "\n" };
    Ok(format!("---\n{yaml}{newline}---\n{}", t.body))
}

impl Tracker for FsTracker {
    fn preflight(&self) -> Result<String> {
        ensure!(
            self.dir.is_dir(),
            "no tickets folder at {} — create it ([tracker] path)",
            self.dir.display()
        );
        let (good, bad) = self.scan()?;
        if let Some((path, e)) = bad.first() {
            bail!("{} bad ticket file(s), first: {}: {e:#}", bad.len(), path.display());
        }
        Ok(format!("{} · {} tickets ({}-N) · claims as {}", self.dir.display(), good.len(), self.prefix, self.user))
    }

    fn board(&self) -> Result<Vec<TrackerTicket>> {
        let (good, bad) = self.scan()?;
        for (path, e) in &bad {
            eprintln!("fleetd: tracker: skipping {}: {e:#}", path.display());
        }
        Ok(good.into_iter().map(|(key, t)| self.to_ticket(key, &t)).collect())
    }

    fn get(&self, key: &TicketKey) -> Result<TrackerTicket> {
        Ok(self.to_ticket(key.clone(), &self.load(key)?))
    }

    fn transition(&self, key: &TicketKey, to: TicketStatus) -> Result<()> {
        self.update(key, |h| h.status = to)
    }

    fn assigned_to_peer(&self, key: &TicketKey) -> Result<bool> {
        Ok(self.load(key)?.header.assignee.is_some_and(|a| a != self.user))
    }

    fn claim(&self, key: &TicketKey) -> Result<()> {
        let user = self.user.clone();
        self.update(key, |h| h.assignee = Some(user))
    }

    fn parent_of(&self, key: &TicketKey) -> Result<Option<TicketKey>> {
        Ok(self.load(key)?.header.parent.as_deref().and_then(|p| TicketKey::parse(p).ok()))
    }

    fn subtasks(&self, parent: &TicketKey) -> Result<Vec<TicketStatus>> {
        let (good, _) = self.scan()?;
        Ok(good.iter().filter(|(_, t)| t.header.parent.as_deref() == Some(&parent.0)).map(|(_, t)| t.header.status).collect())
    }

    fn read(&self, key: &TicketKey) -> Result<TicketDetail> {
        let t = self.load(key)?;
        Ok(TicketDetail {
            kind: t.header.kind.clone(),
            description: t.body.clone(),
            ticket: self.to_ticket(key.clone(), &t),
            held: None,
            location: self.path(key).to_string_lossy().into_owned(),
        })
    }

    fn create(&self, d: &TicketDraft) -> Result<TicketKey> {
        // The next number after every <PREFIX>-N file name in the folder, bad files included, so a
        // new ticket never takes the name of one that failed to parse.
        let names = std::fs::read_dir(&self.dir).with_context(|| format!("reading tickets in {}", self.dir.display()))?;
        let last = names
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter_map(|p| self.key_of(&p).ok())
            .filter_map(|k| k.0.rsplit_once('-').and_then(|(_, n)| n.parse::<u64>().ok()))
            .max()
            .unwrap_or(0);
        let key = TicketKey(format!("{}-{}", self.prefix, last + 1));
        let ticket = TicketFile {
            header: Header {
                title: d.title.clone(),
                kind: d.kind.clone(),
                status: TicketStatus::Todo,
                convoy: d.convoy.as_ref().map(|c| c.0.clone()),
                blocked_by: d.blocked_by.iter().map(|b| b.0.clone()).collect(),
                assignee: None,
                parent: None,
            },
            body: d.description.clone(),
        };
        // create_new: fail rather than overwrite if the name was taken in the meantime.
        let path = self.path(&key);
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .with_context(|| format!("creating {}", path.display()))?;
        std::io::Write::write_all(&mut file, render(&ticket)?.as_bytes())?;
        Ok(key)
    }

    fn block(&self, key: &TicketKey, by: &TicketKey) -> Result<()> {
        self.update(key, |h| {
            if !h.blocked_by.contains(&by.0) {
                h.blocked_by.push(by.0.clone());
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ARMT_4: &str = "---\ntitle: Add neg\ntype: story\nstatus: todo\nconvoy: ARMT-1\nblocked_by: [ARMT-3]\n---\n## Outcome\n\nneg works.\n";

    fn tracker(tag: &str, files: &[(&str, &str)]) -> (FsTracker, PathBuf) {
        let dir = std::env::temp_dir().join(format!("fs-tracker-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for (name, text) in files {
            std::fs::write(dir.join(name), text).unwrap();
        }
        (FsTracker::new(&dir, "ARMT", "daniel"), dir)
    }

    #[test]
    fn reads_a_ticket_file_into_the_board() {
        let (t, dir) = tracker("board", &[("ARMT-4.md", ARMT_4), ("README.txt", "not a ticket")]);
        let board = t.board().unwrap();
        assert_eq!(board.len(), 1);
        let b = &board[0];
        assert_eq!((b.key.0.as_str(), b.title.as_str(), b.status), ("ARMT-4", "Add neg", TicketStatus::Todo));
        assert_eq!(b.convoy, Some(ConvoyKey("ARMT-1".into())));
        assert_eq!(b.blocked_by, vec![TicketKey("ARMT-3".into())]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn writes_rewrite_only_the_header_and_keep_the_body() {
        let (t, dir) = tracker("write", &[("ARMT-4.md", ARMT_4)]);
        let key = TicketKey("ARMT-4".into());
        assert!(!t.assigned_to_peer(&key).unwrap(), "unassigned is claimable");
        t.transition(&key, TicketStatus::InProgress).unwrap();
        t.claim(&key).unwrap();
        t.claim(&key).unwrap(); // idempotent
        let text = std::fs::read_to_string(dir.join("ARMT-4.md")).unwrap();
        assert!(text.ends_with("---\n## Outcome\n\nneg works.\n"), "body kept byte for byte: {text}");
        let got = t.get(&key).unwrap();
        assert_eq!((got.status, got.assignee.as_deref()), (TicketStatus::InProgress, Some("daniel")));
        assert!(!t.assigned_to_peer(&key).unwrap(), "assigned to me isn't a peer");
        let peer = FsTracker::new(&dir, "ARMT", "someone-else");
        assert!(peer.assigned_to_peer(&key).unwrap(), "assigned to daniel is a peer to someone else");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_bad_file_is_skipped_by_the_board_and_named_by_preflight() {
        let (t, dir) = tracker("bad", &[
            ("ARMT-4.md", ARMT_4),
            ("ARMT-5.md", "no header here"),
            ("OTHER-1.md", ARMT_4), // wrong prefix
        ]);
        assert_eq!(t.board().unwrap().len(), 1, "only the good ticket");
        let e = t.preflight().unwrap_err().to_string();
        assert!(e.contains("2 bad ticket file(s)"), "{e}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn creates_the_next_numbered_ticket_and_blocks_it() {
        let (t, dir) = tracker("create", &[("ARMT-4.md", ARMT_4), ("ARMT-9.md", "a bad file still holds its number")]);
        let draft = TicketDraft {
            title: "Add mul".into(),
            kind: "story".into(),
            description: "## Outcome\n\nmul works.\n".into(),
            convoy: Some(ConvoyKey("ARMT-1".into())),
            blocked_by: vec![],
        };
        let key = t.create(&draft).unwrap();
        assert_eq!(key, TicketKey("ARMT-10".into()), "after the highest number, bad files included");
        t.block(&key, &TicketKey("ARMT-4".into())).unwrap();
        t.block(&key, &TicketKey("ARMT-4".into())).unwrap(); // idempotent
        let d = t.read(&key).unwrap();
        assert_eq!((d.kind.as_str(), d.description.as_str()), ("story", "## Outcome\n\nmul works.\n"));
        assert_eq!((d.ticket.status, d.ticket.blocked_by.clone()), (TicketStatus::Todo, vec![TicketKey("ARMT-4".into())]));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn subtasks_are_the_tickets_naming_the_parent() {
        let child = |status: &str| format!("---\ntitle: c\ntype: task\nstatus: {status}\nparent: ARMT-1\n---\n");
        let (t, dir) = tracker("subtasks", &[("ARMT-1.md", ARMT_4), ("ARMT-2.md", &child("done")), ("ARMT-3.md", &child("in_qa"))]);
        let mut subs = t.subtasks(&TicketKey("ARMT-1".into())).unwrap();
        subs.sort_by_key(|s| s.as_str());
        assert_eq!(subs, vec![TicketStatus::Done, TicketStatus::InQa]);
        assert_eq!(t.parent_of(&TicketKey("ARMT-2".into())).unwrap(), Some(TicketKey("ARMT-1".into())));
        let _ = std::fs::remove_dir_all(dir);
    }
}
