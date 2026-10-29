//! wire — the over-the-UDS contract shared by `fleetd` (server) and its clients
//! (`fleetc`, and the read TUIs later). PLAN §2.1.
//!
//! HTTP-over-UDS: tiny request/response DTOs (serde JSON) + a blocking client that
//! sends one request over a `UnixStream`. No async — the socket is a local file, one
//! round-trip per call. `wire` is pure transport: the DTOs are plain strings, and
//! both ends validate against `domain` (client = fail-fast, server = authoritative),
//! so `wire` never depends on `domain`.

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;

/// A crew member's status report as it travels the wire (the ported status-drop). `target` names
/// the reporting cog the way the control verbs do: its ticket, a captain's convoy, or the lookout.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StatusReport {
    #[serde(flatten)]
    pub target: CogTarget,
    pub state: String,
    pub last_action: Option<String>,
}

/// A dispatch order as it travels the wire — the modern dispatch packet
/// (the operator's intent to build a ticket). `created_at` is stamped server-side.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DispatchOrder {
    pub ticket: String,
    pub auto_impl: bool,   // true = worker · false = hand
    /// The ref to fork off; `None` = origin's default branch, filled in by fleetd.
    #[serde(default)]
    pub fork_from: Option<String>,
    pub ac_summary: String,
    pub park_at: Option<String>,
    #[serde(default)]
    pub worktree_path: Option<String>,
}

/// A convoy open/close command as it travels the wire. `open` carries an optional `title` and
/// `plan` (its text; none = no plan); `close` ignores both. The verb is the route (`/convoy-open`
/// vs `/convoy-close`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConvoyCmd {
    pub key: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub plan: Option<String>,
}

/// Add a ticket to a convoy, or take an added one out (`POST /convoy-add`, `/convoy-remove`;
/// `fleetc convoy add|remove <CONVOY> <TICKET> [--hand]`). `hand` (add only): a hand works it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConvoyMemberCmd {
    pub convoy: String,
    pub ticket: String,
    #[serde(default)]
    pub hand: bool,
}

/// A cog-targeted control command (anchor / weigh / sink). `cog` is the required kind; the key is
/// per-cog — worker/sailor/hand carry a `ticket`, a captain a `convoy`, the lookout neither. `/anchor`
/// drops a graceful stop + hold on the live cog; `/weigh` lifts it; `/sink` scuttles it (hard kill +
/// end the session). The daemon branches on `cog` to resolve the target.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CogTarget {
    pub cog: String,
    #[serde(default)]
    pub ticket: Option<String>,
    #[serde(default)]
    pub convoy: Option<String>,
}

/// Anchor a ticket (`POST /anchor-ticket`, `fleetc anchor <KEY>`): hold everything on it — the
/// ticket off the frontier, its MR disarmed + drafted, its live cogs stopped — with the `reason`.
/// `POST /weigh-ticket` releases it all (`reason` ignored). The captain's groom-gate; the operator's.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TicketAnchor {
    pub ticket: String,
    #[serde(default)]
    pub reason: Option<String>,
}

/// Rework a ticket (`POST /rework`, `fleetc rework <KEY>`): send it back down the line against its
/// changed ACs — a worker, or a `hand`, picks it up again in its worktree, on its branch and MR.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TicketRework {
    pub ticket: String,
    #[serde(default)]
    pub hand: bool,
}

/// Launch a ship — open the MR for the branch checked out in `worktree` (`POST /launch`). The
/// target is `target`, else the ticket's order fork base, else origin's default branch.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LaunchCmd {
    #[serde(default)]
    pub ticket: Option<String>,
    pub worktree: String,
    #[serde(default)]
    pub target: Option<String>,
    pub title: String,
    pub description: String,
    #[serde(default)]
    pub draft: bool,
}

/// Post one review comment on an MR, approving it when `approve` (`POST /review-post`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReviewPostCmd {
    pub iid: i64,
    pub body: String,
    #[serde(default)]
    pub approve: bool,
}

/// Reply on one review thread, resolving it when `resolve` (`POST /reply`, `fleetc reply`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReplyCmd {
    pub iid: i64,
    pub thread: String,
    pub body: String,
    #[serde(default)]
    pub resolve: bool,
}

/// Arm a ticket's open MR to merge once its gates pass (`POST /arm`, `fleetc arm`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArmCmd {
    pub ticket: String,
}

/// Create a ticket (`POST /ticket-new`, `fleetc ticket new`): the tracker picks the key.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TicketNewCmd {
    pub title: String,
    pub kind: String,
    pub description: String,
    #[serde(default)]
    pub convoy: Option<String>,
    #[serde(default)]
    pub blocked_by: Vec<String>,
}

/// Propose work (`POST /propose`, `fleetc propose`): held in the ledger for the operator to accept
/// or drop. A new ticket is filed at once, labelled `fleet-proposed` and outside any convoy, so the
/// cog has a key to cite. `from_ticket` is the proposing cog's ticket (its `FLEET_TICKET`); either
/// shape lands in that ticket's convoy on accept unless it names one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProposeCmd {
    #[serde(default)]
    pub from_ticket: Option<String>,
    pub proposed: ProposedCmd,
}

/// A new ticket to file, or an existing one to take on and why. Either names the convoy
/// it lands in (a new one in its draft), or inherits the proposing ticket's.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProposedCmd {
    New(TicketNewCmd),
    Existing {
        ticket: String,
        why: String,
        #[serde(default)]
        convoy: Option<String>,
    },
}

/// Settle a proposal: accept it (`POST /proposal-accept` — the ticket is official, optionally in
/// another convoy than it proposed) or drop it (`POST /proposal-drop` — a ticket filed for it is
/// cancelled; `convoy` ignored).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProposalCmd {
    pub id: i64,
    #[serde(default)]
    pub convoy: Option<String>,
}

/// Withdraw a ticket from the fleet for good (`POST /decommission`, `fleetc decommission`): close
/// its open MR, cancel it with `reason` as a comment, and reap its cogs, worktree and order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DecommissionCmd {
    pub ticket: String,
    pub reason: String,
}

/// Record that `ticket` is blocked by `by` (`POST /ticket-block`, `fleetc ticket block`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TicketBlockCmd {
    pub ticket: String,
    pub by: String,
}

/// A ticket's text: its new description (`POST /ticket-edit`, `fleetc ticket edit` — replaces it
/// whole) or a comment on it (`POST /ticket-comment`, `fleetc ticket comment`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TicketTextCmd {
    pub ticket: String,
    pub body: String,
}

/// An operator message pushed into a cog's tmux pane (`POST /send`). The target is the same per-cog
/// addressing as the control verbs (worker/sailor/hand by `ticket`, a captain by `convoy`, the
/// lookout as the singleton); `message` is typed into the pane and submitted (send-keys + Enter), so
/// the cog receives it as a prompt. No DB gate — it's a nudge, not a hold.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SendMsg {
    pub cog: String,
    #[serde(default)]
    pub ticket: Option<String>,
    #[serde(default)]
    pub convoy: Option<String>,
    pub message: String,
}

/// A ticketless hand launch (`POST /hand`): spawn an operator-driven hand in a worktree with NO
/// ticket and NO order — a tracked scratch cog (dashboard-visible, liveness-probed), distinct from
/// a ticketed `dispatch --hand`. The operator drives it; reconcile leaves ticketless cogs alone.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HandLaunch {
    pub worktree_path: String,
}

/// A live-config overwrite (`POST /settings`): set one lever's `value`. The daemon validates the
/// key is a known lever and the value parses to its type before absorbing it (shape check).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SettingCmd {
    pub key: String,
    pub value: String,
}

/// A fleet project: the folder holding `fleet.toml`, with its runtime state under `.fleet/`. Every
/// client — fleetd, fleetc, the dashboard, a cog — finds it the same way: from a start directory
/// (the cwd, or `-C <dir>`), the nearest folder, itself or a parent, holding `fleet.toml`. Outside
/// a project there is no fleet to talk to, so `find` fails rather than guess.
#[derive(Debug, Clone, PartialEq)]
pub struct Project {
    pub root: std::path::PathBuf,
}

impl Project {
    pub const CONFIG: &'static str = "fleet.toml";

    /// The project enclosing `start` (default: the cwd).
    pub fn find(start: Option<&std::path::Path>) -> Result<Self> {
        let start = match start {
            Some(dir) => std::fs::canonicalize(dir).map_err(|e| anyhow!("-C {}: {e}", dir.display()))?,
            None => std::env::current_dir()?,
        };
        start
            .ancestors()
            .find(|dir| dir.join(Self::CONFIG).is_file())
            .map(|root| Self { root: root.to_path_buf() })
            .ok_or_else(|| {
                anyhow!(
                    "not inside a fleet project: no {} in {} or any parent — cd into projects/<name>, or pass -C projects/<name>",
                    Self::CONFIG,
                    start.display()
                )
            })
    }

    pub fn config(&self) -> std::path::PathBuf {
        self.root.join(Self::CONFIG)
    }
    /// `.fleet/` — the runtime state fleetd owns.
    pub fn runtime(&self) -> std::path::PathBuf {
        self.root.join(".fleet")
    }
    pub fn socket(&self) -> String {
        self.runtime().join("fleetd.sock").to_string_lossy().into_owned()
    }
    pub fn ledger(&self) -> String {
        self.runtime().join("ledger.db").to_string_lossy().into_owned()
    }
    pub fn telemetry_db(&self) -> String {
        self.runtime().join("telemetry.db").to_string_lossy().into_owned()
    }
    /// fleetd's log — one line per event; the previous run's is kept as `fleetd.log.1`.
    pub fn log(&self) -> std::path::PathBuf {
        self.runtime().join("fleetd.log")
    }
}

/// Split a leading `-C <dir>` off a binary's args (like `git -C`): the directory to find the
/// project from, and the remaining args.
pub fn split_dir_flag(mut args: Vec<String>) -> Result<(Option<std::path::PathBuf>, Vec<String>)> {
    if args.first().map(String::as_str) != Some("-C") {
        return Ok((None, args));
    }
    args.remove(0);
    if args.is_empty() {
        return Err(anyhow!("-C needs a directory"));
    }
    let dir = std::path::PathBuf::from(args.remove(0));
    Ok((Some(dir), args))
}

/// POST a JSON body to `path` on the fleetd socket; returns `(status_code, body)`.
/// GET `path` over the UDS and return `(status, body)` — the read twin of `post_json` (no body).
pub fn get_json(socket: &str, path: &str) -> Result<(u16, String)> {
    let req = format!("GET {path} HTTP/1.1\r\nHost: fleetd\r\nConnection: close\r\n\r\n");
    send(socket, &req)
}

pub fn post_json<T: Serialize>(socket: &str, path: &str, body: &T) -> Result<(u16, String)> {
    let payload = serde_json::to_string(body)?;
    let req = format!(
        "POST {path} HTTP/1.1\r\nHost: fleetd\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{payload}",
        payload.len()
    );
    send(socket, &req)
}

fn send(socket: &str, raw: &str) -> Result<(u16, String)> {
    let mut stream = UnixStream::connect(socket)
        .map_err(|e| anyhow!("fleetd not reachable at {socket}: {e}"))?;
    stream.write_all(raw.as_bytes())?;
    let mut resp = String::new();
    stream.read_to_string(&mut resp)?;
    parse_response(&resp)
}

/// Split an HTTP response into `(status_code, body)`.
fn parse_response(resp: &str) -> Result<(u16, String)> {
    let code = resp
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|c| c.parse().ok())
        .ok_or_else(|| anyhow!("malformed response: {resp}"))?;
    let body = resp.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
    Ok((code, body))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_is_the_nearest_folder_holding_fleet_toml() {
        let tmp = std::env::temp_dir().join(format!("wire-project-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let deep = tmp.join("projects/p/chore-X-1-foo/src");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(tmp.join("projects/p/fleet.toml"), "").unwrap();
        let root = std::fs::canonicalize(tmp.join("projects/p")).unwrap();

        let p = Project::find(Some(&deep)).unwrap();
        assert_eq!(p.root, root, "found from a worktree, any depth down");
        assert!(p.socket().ends_with("projects/p/.fleet/fleetd.sock"));
        assert!(p.ledger().ends_with("projects/p/.fleet/ledger.db"));

        let e = Project::find(Some(&tmp)).unwrap_err().to_string();
        assert!(e.contains("not inside a fleet project"), "{e}");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn splits_a_leading_dir_flag() {
        let args = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let (dir, rest) = split_dir_flag(args(&["-C", "projects/p", "status", "--x"])).unwrap();
        assert_eq!((dir.unwrap(), rest), (std::path::PathBuf::from("projects/p"), args(&["status", "--x"])));
        assert_eq!(split_dir_flag(args(&["status"])).unwrap(), (None, args(&["status"])));
        assert!(split_dir_flag(args(&["-C"])).is_err());
    }

    #[test]
    fn parses_status_line_and_body() {
        let resp = "HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok";
        assert_eq!(parse_response(resp).unwrap(), (200, "ok".to_string()));
    }

    #[test]
    fn parses_error_code() {
        let resp = "HTTP/1.1 409 Conflict\r\n\r\nno live worker";
        let (code, body) = parse_response(resp).unwrap();
        assert_eq!(code, 409);
        assert_eq!(body, "no live worker");
    }
}
