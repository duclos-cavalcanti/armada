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

/// A crew member's status report as it travels the wire (the ported status-drop).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StatusReport {
    pub cog: String,
    pub ticket: String,
    pub state: String,
    pub last_action: Option<String>,
}

/// A dispatch order as it travels the wire — the modern dispatch packet
/// (the operator's intent to build a ticket). `created_at` is stamped server-side.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DispatchOrder {
    pub ticket: String,
    pub auto_impl: bool,   // true = worker · false = hand
    pub auto_review: bool, // true = auto-merge · false = held in draft for review
    pub fork_from: String,
    pub ac_summary: String,
    pub park_at: Option<String>,
    #[serde(default)]
    pub worktree_path: Option<String>,
}

/// A convoy open/close command as it travels the wire. `open` carries an optional `title`;
/// `close` ignores it. The verb is the route (`/convoy-open` vs `/convoy-close`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConvoyCmd {
    pub key: String,
    #[serde(default)]
    pub title: Option<String>,
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

/// A ticket hold command. `/hold` sets `held` (keeping the ticket off the dispatch frontier) and
/// carries the groomer's `reason`; `/unhold` releases it (the route decides, so `reason` is ignored
/// there). The captain's groom-gate uses this; so can the operator.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HoldCmd {
    pub ticket: String,
    #[serde(default)]
    pub reason: Option<String>,
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

/// The default socket path (`$HOME/.fleet/fleetd.sock`) — the one place the daemon and
/// every client agree on where the door is.
pub fn default_socket() -> String {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    format!("{home}/.fleet/fleetd.sock")
}

/// The default telemetry DB path (`$HOME/.fleet/telemetry.db`) — written by fleetd's collector,
/// read by `fleetc cost`; one definition shared by writer + reader.
pub fn default_telemetry_db() -> String {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    format!("{home}/.fleet/telemetry.db")
}

/// The default operational ledger path (`$HOME/.fleet/ledger.db`) — fleetd's single-writer DB,
/// read-only by `fleetc plan`; one definition shared by the daemon config + the reader.
pub fn default_ledger() -> String {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    format!("{home}/.fleet/ledger.db")
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
