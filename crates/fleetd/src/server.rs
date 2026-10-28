//! A tiny HTTP-over-UDS server (PLAN §5). Reads serve the in-memory snapshot; the one
//! write route (`POST /status`) relays to the single-writer scheduler over a channel.
//!
//! HTTP is the *language* (GET /context → JSON); a Unix-domain socket *file* is the
//! *door* (no TCP port; auth = filesystem permissions; SSH-forwardable later). It
//! doesn't justify an async runtime — a `UnixListener` + a thread per connection is
//! plenty. Reads never touch the ledger (they serve the poll's snapshot); the write
//! route never touches the ledger *here* either — it hands a `Command` to the writer.

use crate::scheduler::{Command, ControlTarget};
use crate::Snapshot;
use anyhow::Result;
use domain::{CogState, ConvoyKey, CrewKind, Order, TicketKey};
use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::mpsc::Sender;

/// The deployment facts fixed at startup, answered without touching the writer: the project
/// checkout (`GET /project`) and the defaults a dispatch that omits them inherits.
pub struct Deployment {
    /// The resolved `[project] path`.
    pub project: String,
    /// origin's default branch (`origin/main`), read from the project checkout at boot.
    pub default_branch: String,
    /// The forge can stack MRs (`Forge::stacks`). Without it a dispatch must fork the default branch.
    pub stacks: bool,
}

/// Refuse to start when another fleetd already answers on `path` — one writer per ledger. A socket
/// file nobody answers is a crash's leftover, which `serve` clears.
pub fn refuse_if_running(path: &str) -> Result<()> {
    anyhow::ensure!(UnixStream::connect(path).is_err(), "fleetd is already running on {path}");
    Ok(())
}

/// Bind the socket (clearing any stale file) and serve forever. `cmds` is the write
/// channel to the scheduler; each connection gets its own clone.
pub fn serve(path: &str, snapshot: Snapshot, cmds: Sender<Command>, deployment: Deployment) -> Result<()> {
    let deployment = std::sync::Arc::new(deployment);
    let _ = std::fs::remove_file(path); // a leftover socket from a crash would block bind
    let listener = UnixListener::bind(path)?;
    for stream in listener.incoming() {
        match stream {
            Ok(mut s) => {
                let snapshot = snapshot.clone();
                let cmds = cmds.clone();
                let deployment = deployment.clone();
                std::thread::spawn(move || {
                    if let Err(e) = handle(&mut s, &snapshot, &cmds, &deployment) {
                        tracing::warn!("connection error: {e}");
                    }
                });
            }
            Err(e) => tracing::warn!("accept error: {e}"),
        }
    }
    Ok(())
}

fn handle(
    stream: &mut UnixStream,
    snapshot: &Snapshot,
    cmds: &Sender<Command>,
    deployment: &Deployment,
) -> std::io::Result<()> {
    let request = read_request(stream)?;
    let (method, path) = parse_request_line(&request);

    let response = match (method.as_deref(), path.as_deref()) {
        (Some("GET"), Some(p)) if p.starts_with("/context?convoy=") => handle_convoy_view(cmds, p),
        (Some("GET"), Some("/context")) => {
            let body = serde_json::to_string(&*snapshot.lock().unwrap())
                .unwrap_or_else(|_| "[]".into());
            http_response("200 OK", "application/json", &body)
        }
        (Some("GET"), Some("/health")) => http_response("200 OK", "text/plain", "ok"),
        (Some("GET"), Some("/project")) => http_response("200 OK", "text/plain", &deployment.project),
        (Some("POST"), Some("/status")) => handle_status(&request, cmds),
        (Some("POST"), Some("/dispatch")) => handle_dispatch(&request, cmds, deployment),
        (Some("POST"), Some("/hand")) => handle_hand(&request, cmds),
        (Some("POST"), Some("/convoy-open")) => handle_convoy(&request, cmds, true),
        (Some("POST"), Some("/convoy-close")) => handle_convoy(&request, cmds, false),
        (Some("POST"), Some("/convoy-add")) => handle_convoy_member(&request, cmds, true),
        (Some("POST"), Some("/convoy-remove")) => handle_convoy_member(&request, cmds, false),
        (Some("POST"), Some("/anchor")) => handle_hold(&request, cmds, Hold::Anchor),
        (Some("POST"), Some("/weigh")) => handle_hold(&request, cmds, Hold::Weigh),
        (Some("POST"), Some("/sink")) => handle_hold(&request, cmds, Hold::Sink),
        (Some("POST"), Some("/send")) => handle_send(&request, cmds),
        (Some("POST"), Some("/hold")) => handle_set_held(&request, cmds, true),
        (Some("POST"), Some("/unhold")) => handle_set_held(&request, cmds, false),
        (Some("GET"), Some(p)) if p.starts_with("/packet") => handle_packet(cmds, p),
        (Some("GET"), Some(p)) if p.starts_with("/review") => handle_review(cmds, p),
        (Some("POST"), Some(p)) if p.starts_with("/rebase") => handle_rebase(cmds, p),
        (Some("GET"), Some(p)) if p.starts_with("/ci?") => handle_ci(cmds, p),
        (Some("POST"), Some("/reply")) => handle_reply(&request, cmds),
        (Some("GET"), Some(p)) if p.starts_with("/ship?") => handle_ship(cmds, p),
        (Some("POST"), Some("/review-post")) => handle_review_post(&request, cmds),
        (Some("POST"), Some(p)) if p.starts_with("/retry-ci?") => handle_retry_ci(cmds, p),
        (Some("POST"), Some("/launch")) => handle_launch(&request, cmds, deployment),
        (Some("POST"), Some("/arm")) => handle_arm(&request, cmds),
        (Some("GET"), Some(p)) if p.starts_with("/ticket?") => handle_ticket_view(cmds, p),
        (Some("POST"), Some("/ticket-new")) => handle_ticket_new(&request, cmds),
        (Some("POST"), Some("/ticket-block")) => handle_ticket_block(&request, cmds),
        (Some("POST"), Some("/ticket-edit")) => handle_ticket_text(&request, cmds, false),
        (Some("POST"), Some("/ticket-comment")) => handle_ticket_text(&request, cmds, true),
        (Some("POST"), Some("/propose")) => handle_propose(&request, cmds),
        (Some("POST"), Some("/decommission")) => handle_decommission(&request, cmds),
        (Some("POST"), Some("/proposal-accept")) => handle_proposal(&request, cmds, true),
        (Some("POST"), Some("/proposal-drop")) => handle_proposal(&request, cmds, false),
        (Some("GET"), Some("/settings")) => handle_settings_list(cmds),
        (Some("POST"), Some("/settings")) => handle_setting(&request, cmds),
        _ => http_response("404 Not Found", "text/plain", "not found"),
    };
    stream.write_all(response.as_bytes())
}

/// `POST /status` — parse the wire DTO, validate it authoritatively (the cog + state
/// must parse into `domain` types), relay to the writer, and await its outcome.
fn handle_status(request: &str, cmds: &Sender<Command>) -> String {
    let body = request.split("\r\n\r\n").nth(1).unwrap_or("");
    let dto: wire::StatusReport = match serde_json::from_str(body) {
        Ok(d) => d,
        Err(e) => return http_response("400 Bad Request", "text/plain", &format!("bad json: {e}")),
    };
    let kind = match CrewKind::parse(&dto.cog) {
        Some(k) => k,
        None => return http_response("400 Bad Request", "text/plain", &format!("invalid cog: {}", dto.cog)),
    };
    let state = match CogState::parse(kind, &dto.state) {
        Some(s) => s,
        None => {
            let msg = format!("invalid state '{}' for cog '{}'", dto.state, dto.cog);
            return http_response("400 Bad Request", "text/plain", &msg);
        }
    };

    // The lookout is a singleton: its report needs no ticket to find its session; every other cog's does.
    let ticket = match (dto.ticket.as_deref().map(parse_ticket).transpose(), kind) {
        (Err(r), _) => return r,
        (Ok(None), k) if k != CrewKind::Lookout => {
            return http_response("400 Bad Request", "text/plain", &format!("{} reports need a ticket", dto.cog));
        }
        (Ok(t), _) => t,
    };
    let (reply_tx, reply_rx) = std::sync::mpsc::channel();
    let cmd = Command::RecordStatus {
        cog: kind,
        ticket,
        state,
        last_action: dto.last_action,
        reply: reply_tx,
    };
    if cmds.send(cmd).is_err() {
        return http_response("503 Service Unavailable", "text/plain", "writer unavailable");
    }
    match reply_rx.recv() {
        Ok(Ok(())) => http_response("200 OK", "text/plain", "ok"),
        Ok(Err(e)) => http_response("409 Conflict", "text/plain", &e),
        Err(_) => http_response("503 Service Unavailable", "text/plain", "no reply from writer"),
    }
}

/// Parse a client-sent ticket key — or the `400` response to return. Keys are validated at
/// the edge (`<PREFIX>-<n>`), never passed through raw.
/// Largest request accepted — far above any real packet (an MR description is a few KB).
const MAX_REQUEST: usize = 1 << 20;

/// Read one request: the head, then its `Content-Length` body. A single `read` would cut a body
/// that spans buffers (an MR description); the client keeps its end open, so EOF can't delimit it.
fn read_request(stream: &mut impl Read) -> std::io::Result<String> {
    let (mut buf, mut chunk) = (Vec::new(), [0u8; 4096]);
    loop {
        let n = stream.read(&mut chunk)?;
        buf.extend_from_slice(&chunk[..n]);
        let done = buf.windows(4).position(|w| w == b"\r\n\r\n").is_some_and(|end| {
            let head = String::from_utf8_lossy(&buf[..end]).to_ascii_lowercase();
            let len = head
                .lines()
                .find_map(|l| l.strip_prefix("content-length:"))
                .and_then(|v| v.trim().parse::<usize>().ok())
                .unwrap_or(0);
            buf.len() >= end + 4 + len
        });
        if n == 0 || done || buf.len() > MAX_REQUEST {
            return Ok(String::from_utf8_lossy(&buf).into_owned());
        }
    }
}

fn parse_ticket(s: &str) -> Result<TicketKey, String> {
    TicketKey::parse(s).map_err(|e| http_response("400 Bad Request", "text/plain", &e.to_string()))
}

/// `POST /dispatch` — parse the order packet, stamp `created_at`, relay to the writer.
/// The ledger's FK (ticket must exist on the board) is the authoritative check.
fn handle_dispatch(request: &str, cmds: &Sender<Command>, deployment: &Deployment) -> String {
    let body = request.split("\r\n\r\n").nth(1).unwrap_or("");
    let dto: wire::DispatchOrder = match serde_json::from_str(body) {
        Ok(d) => d,
        Err(e) => return http_response("400 Bad Request", "text/plain", &format!("bad json: {e}")),
    };
    let ticket = match parse_ticket(&dto.ticket) {
        Ok(t) => t,
        Err(r) => return r,
    };
    let fork_from = dto.fork_from.unwrap_or_else(|| deployment.default_branch.clone());
    let branch = |r: &str| r.strip_prefix("origin/").unwrap_or(r).to_string();
    if !deployment.stacks && branch(&fork_from) != branch(&deployment.default_branch) {
        let why = format!(
            "this forge can't stack MRs: fork {} instead of {fork_from} (or wait for the parent to merge)",
            deployment.default_branch
        );
        return http_response("400 Bad Request", "text/plain", &why);
    }
    let order = Order {
        ticket,
        auto_impl: dto.auto_impl,
        fork_from,
        ac_summary: dto.ac_summary,
        park_at: dto.park_at,
        worktree_path: dto.worktree_path,
        created_at: crate::now_rfc3339(),
    };
    let (reply_tx, reply_rx) = std::sync::mpsc::channel();
    if cmds.send(Command::Dispatch { order, reply: reply_tx }).is_err() {
        return http_response("503 Service Unavailable", "text/plain", "writer unavailable");
    }
    match reply_rx.recv() {
        Ok(Ok(())) => http_response("200 OK", "text/plain", "ok"),
        Ok(Err(e)) => http_response("409 Conflict", "text/plain", &e),
        Err(_) => http_response("503 Service Unavailable", "text/plain", "no reply from writer"),
    }
}

/// `POST /hand` — launch a ticketless operator-driven hand in a worktree (no order, no ticket).
fn handle_hand(request: &str, cmds: &Sender<Command>) -> String {
    let body = request.split("\r\n\r\n").nth(1).unwrap_or("");
    let dto: wire::HandLaunch = match serde_json::from_str(body) {
        Ok(d) => d,
        Err(e) => return http_response("400 Bad Request", "text/plain", &format!("bad json: {e}")),
    };
    let (reply_tx, reply_rx) = std::sync::mpsc::channel();
    if cmds.send(Command::LaunchHand { worktree_path: dto.worktree_path, reply: reply_tx }).is_err() {
        return http_response("503 Service Unavailable", "text/plain", "writer unavailable");
    }
    match reply_rx.recv() {
        Ok(Ok(())) => http_response("200 OK", "text/plain", "ok"),
        Ok(Err(e)) => http_response("409 Conflict", "text/plain", &e),
        Err(_) => http_response("503 Service Unavailable", "text/plain", "no reply from writer"),
    }
}

/// `POST /convoy-open` | `/convoy-close` — parse the convoy command and relay to the writer.
/// `open=true` opens (idempotent create, optional title); `open=false` closes (the captain's
/// "all delivered" sign-off → reconcile reaps the captain next tick).
fn handle_convoy(request: &str, cmds: &Sender<Command>, open: bool) -> String {
    let body = request.split("\r\n\r\n").nth(1).unwrap_or("");
    let dto: wire::ConvoyCmd = match serde_json::from_str(body) {
        Ok(d) => d,
        Err(e) => return http_response("400 Bad Request", "text/plain", &format!("bad json: {e}")),
    };
    let key = match ConvoyKey::parse(&dto.key) {
        Ok(k) => k,
        Err(e) => return http_response("400 Bad Request", "text/plain", &e.to_string()),
    };
    let (reply_tx, reply_rx) = std::sync::mpsc::channel();
    let cmd = if open {
        Command::OpenConvoy { key, title: dto.title, reply: reply_tx }
    } else {
        Command::CloseConvoy { key, reply: reply_tx }
    };
    if cmds.send(cmd).is_err() {
        return http_response("503 Service Unavailable", "text/plain", "writer unavailable");
    }
    match reply_rx.recv() {
        Ok(Ok(())) => http_response("200 OK", "text/plain", "ok"),
        Ok(Err(e)) => http_response("409 Conflict", "text/plain", &e),
        Err(_) => http_response("503 Service Unavailable", "text/plain", "no reply from writer"),
    }
}

/// `POST /convoy-add` (`add`; reply says what it left) or `/convoy-remove` — a convoy's membership.
fn handle_convoy_member(request: &str, cmds: &Sender<Command>, add: bool) -> String {
    let parsed = parse_body::<wire::ConvoyMemberCmd>(request).and_then(|dto| {
        let convoy = ConvoyKey::parse(&dto.convoy).map_err(|e| http_response("400 Bad Request", "text/plain", &e.to_string()))?;
        Ok((convoy, parse_ticket(&dto.ticket)?))
    });
    match parsed {
        Ok((convoy, ticket)) if add => relay(cmds, |reply| Command::AddToConvoy { convoy, ticket, reply }),
        Ok((convoy, ticket)) => relay(cmds, |reply| Command::RemoveFromConvoy { convoy, ticket, reply }),
        Err(r) => r,
    }
}

/// `GET /context?convoy=PROJ-N` — relay a convoy-view read to the writer and return the convoy's
/// ticket slice (each with `ready`/`dispatched`) as JSON. The captain's boot read.
fn handle_convoy_view(cmds: &Sender<Command>, path: &str) -> String {
    let convoy = path
        .split_once("?convoy=")
        .map(|(_, rest)| rest.split('&').next().unwrap_or("").trim())
        .unwrap_or("");
    if convoy.is_empty() {
        return http_response("400 Bad Request", "text/plain", "missing ?convoy=");
    }
    let convoy = match ConvoyKey::parse(convoy) {
        Ok(k) => k,
        Err(e) => return http_response("400 Bad Request", "text/plain", &e.to_string()),
    };
    let (reply_tx, reply_rx) = std::sync::mpsc::channel();
    let cmd = Command::GetConvoyView { convoy, reply: reply_tx };
    if cmds.send(cmd).is_err() {
        return http_response("503 Service Unavailable", "text/plain", "writer unavailable");
    }
    match reply_rx.recv() {
        Ok(Ok(view)) => {
            let body = serde_json::to_string(&view).unwrap_or_else(|_| "{}".into());
            http_response("200 OK", "application/json", &body)
        }
        Ok(Err(e)) => http_response("409 Conflict", "text/plain", &e),
        Err(_) => http_response("503 Service Unavailable", "text/plain", "no reply from writer"),
    }
}

/// `GET /packet?ticket=PROJ-N` — relay a packet read to the writer (it owns the ledger) and
/// return the composed `{order, ship, snapshot, ticket}` as JSON. The sailor's boot read.
fn handle_packet(cmds: &Sender<Command>, path: &str) -> String {
    let ticket = path
        .split_once("?ticket=")
        .map(|(_, rest)| rest.split('&').next().unwrap_or("").trim())
        .unwrap_or("");
    if ticket.is_empty() {
        return http_response("400 Bad Request", "text/plain", "missing ?ticket=");
    }
    let ticket = match parse_ticket(ticket) {
        Ok(t) => t,
        Err(r) => return r,
    };
    let (reply_tx, reply_rx) = std::sync::mpsc::channel();
    let cmd = Command::GetPacket { ticket, reply: reply_tx };
    if cmds.send(cmd).is_err() {
        return http_response("503 Service Unavailable", "text/plain", "writer unavailable");
    }
    match reply_rx.recv() {
        Ok(Ok(packet)) => {
            let body = serde_json::to_string(&packet).unwrap_or_else(|_| "{}".into());
            http_response("200 OK", "application/json", &body)
        }
        Ok(Err(e)) => http_response("409 Conflict", "text/plain", &e),
        Err(_) => http_response("503 Service Unavailable", "text/plain", "no reply from writer"),
    }
}

/// `GET /review?iid=N` — relay an MR-review read to the writer (it owns the forge) and return the
/// fresh `{open_threads, floating_notes}` as JSON. Replaces the bash mr-open-threads + mr-floating-notes.
fn handle_review(cmds: &Sender<Command>, path: &str) -> String {
    let Some(iid) = parse_iid(path) else {
        return http_response("400 Bad Request", "text/plain", "missing/invalid ?iid=");
    };
    let (reply_tx, reply_rx) = std::sync::mpsc::channel();
    if cmds.send(Command::GetReview { iid, reply: reply_tx }).is_err() {
        return http_response("503 Service Unavailable", "text/plain", "writer unavailable");
    }
    match reply_rx.recv() {
        Ok(Ok(review)) => {
            let body = serde_json::to_string(&review).unwrap_or_else(|_| "{}".into());
            http_response("200 OK", "application/json", &body)
        }
        Ok(Err(e)) => http_response("409 Conflict", "text/plain", &e),
        Err(_) => http_response("503 Service Unavailable", "text/plain", "no reply from writer"),
    }
}

/// `GET /ship?iid=N` — one ship (ours or an ally's) with its snapshot and fetch ref.
fn handle_ship(cmds: &Sender<Command>, path: &str) -> String {
    let Some(iid) = parse_iid(path) else {
        return http_response("400 Bad Request", "text/plain", "missing/invalid ?iid=");
    };
    let (reply_tx, reply_rx) = std::sync::mpsc::channel();
    if cmds.send(Command::GetShip { iid, reply: reply_tx }).is_err() {
        return http_response("503 Service Unavailable", "text/plain", "writer unavailable");
    }
    match reply_rx.recv() {
        Ok(Ok(view)) => http_response("200 OK", "application/json", &serde_json::to_string(&view).unwrap_or_default()),
        Ok(Err(e)) => http_response("409 Conflict", "text/plain", &e),
        Err(_) => http_response("503 Service Unavailable", "text/plain", "no reply from writer"),
    }
}

/// `POST /review-post` — post one review comment on an MR, approving it when asked.
fn handle_review_post(request: &str, cmds: &Sender<Command>) -> String {
    let body = request.split("\r\n\r\n").nth(1).unwrap_or("");
    let cmd: wire::ReviewPostCmd = match serde_json::from_str(body) {
        Ok(d) => d,
        Err(e) => return http_response("400 Bad Request", "text/plain", &format!("bad json: {e}")),
    };
    if cmd.body.trim().is_empty() {
        return http_response("400 Bad Request", "text/plain", "a review needs a body");
    }
    let (reply_tx, reply_rx) = std::sync::mpsc::channel();
    if cmds.send(Command::PostReview { cmd, reply: reply_tx }).is_err() {
        return http_response("503 Service Unavailable", "text/plain", "writer unavailable");
    }
    match reply_rx.recv() {
        Ok(Ok(())) => http_response("200 OK", "text/plain", "posted"),
        Ok(Err(e)) => http_response("409 Conflict", "text/plain", &e),
        Err(_) => http_response("503 Service Unavailable", "text/plain", "no reply from writer"),
    }
}

/// `POST /reply` — answer one review thread, resolving it when asked.
fn handle_reply(request: &str, cmds: &Sender<Command>) -> String {
    let body = request.split("\r\n\r\n").nth(1).unwrap_or("");
    let cmd: wire::ReplyCmd = match serde_json::from_str(body) {
        Ok(d) => d,
        Err(e) => return http_response("400 Bad Request", "text/plain", &format!("bad json: {e}")),
    };
    if cmd.thread.trim().is_empty() || cmd.body.trim().is_empty() {
        return http_response("400 Bad Request", "text/plain", "a reply needs a thread and a body");
    }
    let (reply_tx, reply_rx) = std::sync::mpsc::channel();
    if cmds.send(Command::Reply { cmd, reply: reply_tx }).is_err() {
        return http_response("503 Service Unavailable", "text/plain", "writer unavailable");
    }
    match reply_rx.recv() {
        Ok(Ok(())) => http_response("200 OK", "text/plain", "replied"),
        Ok(Err(e)) => http_response("409 Conflict", "text/plain", &e),
        Err(_) => http_response("503 Service Unavailable", "text/plain", "no reply from writer"),
    }
}

/// `GET /ci?iid=N` — the MR's CI: status + every failed job with its log tail.
fn handle_ci(cmds: &Sender<Command>, path: &str) -> String {
    let Some(iid) = parse_iid(path) else {
        return http_response("400 Bad Request", "text/plain", "missing/invalid ?iid=");
    };
    let (reply_tx, reply_rx) = std::sync::mpsc::channel();
    if cmds.send(Command::GetCi { iid, reply: reply_tx }).is_err() {
        return http_response("503 Service Unavailable", "text/plain", "writer unavailable");
    }
    match reply_rx.recv() {
        Ok(Ok(ci)) => http_response("200 OK", "application/json", &serde_json::to_string(&ci).unwrap_or_default()),
        Ok(Err(e)) => http_response("409 Conflict", "text/plain", &e),
        Err(_) => http_response("503 Service Unavailable", "text/plain", "no reply from writer"),
    }
}

/// `POST /retry-ci?iid=N` — re-run the MR's failed CI jobs.
fn handle_retry_ci(cmds: &Sender<Command>, path: &str) -> String {
    let Some(iid) = parse_iid(path) else {
        return http_response("400 Bad Request", "text/plain", "missing/invalid ?iid=");
    };
    let (reply_tx, reply_rx) = std::sync::mpsc::channel();
    if cmds.send(Command::RetryCi { iid, reply: reply_tx }).is_err() {
        return http_response("503 Service Unavailable", "text/plain", "writer unavailable");
    }
    match reply_rx.recv() {
        Ok(Ok(())) => http_response("200 OK", "text/plain", "retried"),
        Ok(Err(e)) => http_response("409 Conflict", "text/plain", &e),
        Err(_) => http_response("503 Service Unavailable", "text/plain", "no reply from writer"),
    }
}

/// `POST /rebase?iid=N` — relay a server-side rebase to the writer (forge) and return the outcome.
fn handle_rebase(cmds: &Sender<Command>, path: &str) -> String {
    let Some(iid) = parse_iid(path) else {
        return http_response("400 Bad Request", "text/plain", "missing/invalid ?iid=");
    };
    let (reply_tx, reply_rx) = std::sync::mpsc::channel();
    if cmds.send(Command::Rebase { iid, reply: reply_tx }).is_err() {
        return http_response("503 Service Unavailable", "text/plain", "writer unavailable");
    }
    match reply_rx.recv() {
        Ok(Ok(())) => http_response("200 OK", "text/plain", "rebased"),
        Ok(Err(e)) => http_response("409 Conflict", "text/plain", &e),
        Err(_) => http_response("503 Service Unavailable", "text/plain", "no reply from writer"),
    }
}

/// `POST /arm` — arm a ticket's open MR; 409 with the reason when it's held or blocked.
fn handle_arm(request: &str, cmds: &Sender<Command>) -> String {
    let body = request.split("\r\n\r\n").nth(1).unwrap_or("");
    let dto: wire::ArmCmd = match serde_json::from_str(body) {
        Ok(d) => d,
        Err(e) => return http_response("400 Bad Request", "text/plain", &format!("bad json: {e}")),
    };
    let ticket = match parse_ticket(&dto.ticket) {
        Ok(t) => t,
        Err(r) => return r,
    };
    let (reply_tx, reply_rx) = std::sync::mpsc::channel();
    if cmds.send(Command::Arm { ticket, reply: reply_tx }).is_err() {
        return http_response("503 Service Unavailable", "text/plain", "writer unavailable");
    }
    match reply_rx.recv() {
        Ok(Ok(())) => http_response("200 OK", "text/plain", "armed"),
        Ok(Err(e)) => http_response("409 Conflict", "text/plain", &e),
        Err(_) => http_response("503 Service Unavailable", "text/plain", "no reply from writer"),
    }
}

/// `GET /ticket?ticket=<KEY>` — a ticket's full text, read through the tracker.
fn handle_ticket_view(cmds: &Sender<Command>, path: &str) -> String {
    let key = path.split_once("?ticket=").map(|(_, rest)| rest.split('&').next().unwrap_or("").trim()).unwrap_or("");
    let ticket = match parse_ticket(key) {
        Ok(t) => t,
        Err(r) => return r,
    };
    let (reply_tx, reply_rx) = std::sync::mpsc::channel();
    if cmds.send(Command::GetTicket { ticket, reply: reply_tx }).is_err() {
        return http_response("503 Service Unavailable", "text/plain", "writer unavailable");
    }
    match reply_rx.recv() {
        Ok(Ok(detail)) => http_response("200 OK", "application/json", &serde_json::to_string(&detail).unwrap_or_default()),
        Ok(Err(e)) => http_response("409 Conflict", "text/plain", &e),
        Err(_) => http_response("503 Service Unavailable", "text/plain", "no reply from writer"),
    }
}

/// `POST /ticket-new` — create a ticket; reply with its key.
fn handle_ticket_new(request: &str, cmds: &Sender<Command>) -> String {
    let draft = match parse_body(request).and_then(parse_draft) {
        Ok(d) => d,
        Err(r) => return r,
    };
    relay(cmds, |reply| Command::NewTicket { draft, reply })
}

/// `POST /propose` — hold a cog's proposal for the operator; reply with its id.
fn handle_propose(request: &str, cmds: &Sender<Command>) -> String {
    let parsed = parse_body::<wire::ProposeCmd>(request).and_then(|dto| {
        let from = dto.from_ticket.as_deref().map(parse_ticket).transpose()?;
        let proposed = match dto.proposed {
            wire::ProposedCmd::New(draft) => domain::Proposed::New(parse_draft(draft)?),
            wire::ProposedCmd::Existing { ticket, why, convoy } if !why.trim().is_empty() => {
                let convoy = convoy.as_deref().map(ConvoyKey::parse).transpose();
                let convoy = convoy.map_err(|e| http_response("400 Bad Request", "text/plain", &e.to_string()))?;
                domain::Proposed::Existing { ticket: parse_ticket(&ticket)?, why, convoy }
            }
            wire::ProposedCmd::Existing { .. } => {
                return Err(http_response("400 Bad Request", "text/plain", "an existing ticket needs a why"))
            }
        };
        Ok((from, proposed))
    });
    match parsed {
        Ok((from, proposed)) => relay(cmds, |reply| Command::Propose { from, proposed, reply }),
        Err(r) => r,
    }
}

/// `POST /decommission` — withdraw a ticket from the fleet; reply with what was done.
fn handle_decommission(request: &str, cmds: &Sender<Command>) -> String {
    let parsed = parse_body::<wire::DecommissionCmd>(request).and_then(|dto| {
        if dto.reason.trim().is_empty() {
            return Err(http_response("400 Bad Request", "text/plain", "a decommission needs a reason"));
        }
        Ok((parse_ticket(&dto.ticket)?, dto.reason))
    });
    match parsed {
        Ok((ticket, reason)) => relay(cmds, |reply| Command::Decommission { ticket, reason, reply }),
        Err(r) => r,
    }
}

/// `POST /proposal-accept` (`accept`: reply with the ticket it became or was) or `/proposal-drop`.
fn handle_proposal(request: &str, cmds: &Sender<Command>, accept: bool) -> String {
    let dto: wire::ProposalCmd = match parse_body(request) {
        Ok(d) => d,
        Err(r) => return r,
    };
    let id = domain::ProposalId(dto.id);
    if !accept {
        return relay(cmds, |reply| Command::DropProposal { id, reply });
    }
    match dto.convoy.as_deref().map(ConvoyKey::parse).transpose() {
        Ok(convoy) => relay(cmds, |reply| Command::AcceptProposal { id, convoy, reply }),
        Err(e) => http_response("400 Bad Request", "text/plain", &e.to_string()),
    }
}

/// A request's JSON body as `T`; `Err` is the 400 response.
fn parse_body<T: serde::de::DeserializeOwned>(request: &str) -> Result<T, String> {
    let body = request.split("\r\n\r\n").nth(1).unwrap_or("");
    serde_json::from_str(body).map_err(|e| http_response("400 Bad Request", "text/plain", &format!("bad json: {e}")))
}

/// A ticket draft off the wire, validated into `domain`; `Err` is the 400 response.
fn parse_draft(dto: wire::TicketNewCmd) -> Result<domain::TicketDraft, String> {
    let bad = |e: String| http_response("400 Bad Request", "text/plain", &e);
    let convoy = dto.convoy.as_deref().map(ConvoyKey::parse).transpose().map_err(|e| bad(e.to_string()))?;
    let blocked_by = dto.blocked_by.iter().map(|b| parse_ticket(b)).collect::<Result<Vec<_>, _>>()?;
    if dto.title.trim().is_empty() {
        return Err(bad("a ticket needs a title".into()));
    }
    Ok(domain::TicketDraft { title: dto.title, kind: dto.kind, description: dto.description, convoy, blocked_by, labels: vec![] })
}

/// Relay a command to the writer and await its reply: 200 with the reply's text, 409 with the
/// writer's refusal.
fn relay(cmds: &Sender<Command>, cmd: impl FnOnce(Sender<Result<String, String>>) -> Command) -> String {
    let (reply_tx, reply_rx) = std::sync::mpsc::channel();
    if cmds.send(cmd(reply_tx)).is_err() {
        return http_response("503 Service Unavailable", "text/plain", "writer unavailable");
    }
    match reply_rx.recv() {
        Ok(Ok(text)) => http_response("200 OK", "text/plain", &text),
        Ok(Err(e)) => http_response("409 Conflict", "text/plain", &e),
        Err(_) => http_response("503 Service Unavailable", "text/plain", "no reply from writer"),
    }
}

/// `POST /ticket-block` — record that a ticket is blocked by another.
fn handle_ticket_block(request: &str, cmds: &Sender<Command>) -> String {
    let body = request.split("\r\n\r\n").nth(1).unwrap_or("");
    let dto: wire::TicketBlockCmd = match serde_json::from_str(body) {
        Ok(d) => d,
        Err(e) => return http_response("400 Bad Request", "text/plain", &format!("bad json: {e}")),
    };
    let (ticket, by) = match (parse_ticket(&dto.ticket), parse_ticket(&dto.by)) {
        (Ok(t), Ok(b)) => (t, b),
        (Err(r), _) | (_, Err(r)) => return r,
    };
    if ticket == by {
        return http_response("400 Bad Request", "text/plain", "a ticket can't block itself");
    }
    let (reply_tx, reply_rx) = std::sync::mpsc::channel();
    if cmds.send(Command::BlockTicket { ticket, by, reply: reply_tx }).is_err() {
        return http_response("503 Service Unavailable", "text/plain", "writer unavailable");
    }
    match reply_rx.recv() {
        Ok(Ok(())) => http_response("200 OK", "text/plain", "ok"),
        Ok(Err(e)) => http_response("409 Conflict", "text/plain", &e),
        Err(_) => http_response("503 Service Unavailable", "text/plain", "no reply from writer"),
    }
}

/// `POST /ticket-edit` (replace the description) or `/ticket-comment` (`comment`): a ticket's text.
fn handle_ticket_text(request: &str, cmds: &Sender<Command>, comment: bool) -> String {
    let body = request.split("\r\n\r\n").nth(1).unwrap_or("");
    let dto: wire::TicketTextCmd = match serde_json::from_str(body) {
        Ok(d) => d,
        Err(e) => return http_response("400 Bad Request", "text/plain", &format!("bad json: {e}")),
    };
    let ticket = match parse_ticket(&dto.ticket) {
        Ok(t) => t,
        Err(r) => return r,
    };
    if dto.body.trim().is_empty() {
        return http_response("400 Bad Request", "text/plain", "an empty body");
    }
    let (reply_tx, reply_rx) = std::sync::mpsc::channel();
    let cmd = if comment {
        Command::CommentTicket { ticket, body: dto.body, reply: reply_tx }
    } else {
        Command::EditTicket { ticket, description: dto.body, reply: reply_tx }
    };
    if cmds.send(cmd).is_err() {
        return http_response("503 Service Unavailable", "text/plain", "writer unavailable");
    }
    match reply_rx.recv() {
        Ok(Ok(())) => http_response("200 OK", "text/plain", "ok"),
        Ok(Err(e)) => http_response("409 Conflict", "text/plain", &e),
        Err(_) => http_response("503 Service Unavailable", "text/plain", "no reply from writer"),
    }
}

/// `POST /launch` — launch a ship: open the MR for the worktree's branch, reply with its URL.
fn handle_launch(request: &str, cmds: &Sender<Command>, deployment: &Deployment) -> String {
    let body = request.split("\r\n\r\n").nth(1).unwrap_or("");
    let dto: wire::LaunchCmd = match serde_json::from_str(body) {
        Ok(d) => d,
        Err(e) => return http_response("400 Bad Request", "text/plain", &format!("bad json: {e}")),
    };
    let ticket = match dto.ticket.as_deref().map(parse_ticket).transpose() {
        Ok(t) => t,
        Err(r) => return r,
    };
    let launch = crate::scheduler::Launch {
        ticket,
        worktree: dto.worktree,
        target: dto.target,
        default_branch: deployment.default_branch.clone(),
        title: dto.title,
        description: dto.description,
        draft: dto.draft,
    };
    let (reply_tx, reply_rx) = std::sync::mpsc::channel();
    if cmds.send(Command::Launch { launch, reply: reply_tx }).is_err() {
        return http_response("503 Service Unavailable", "text/plain", "writer unavailable");
    }
    match reply_rx.recv() {
        Ok(Ok(url)) => http_response("200 OK", "text/plain", &url),
        Ok(Err(e)) => http_response("409 Conflict", "text/plain", &e),
        Err(_) => http_response("503 Service Unavailable", "text/plain", "no reply from writer"),
    }
}

/// `GET /settings` — relay a live-config read to the writer and return every lever's effective
/// value + source (`{key, value, source}[]`) as JSON. The `fleetc settings list` read.
fn handle_settings_list(cmds: &Sender<Command>) -> String {
    let (reply_tx, reply_rx) = std::sync::mpsc::channel();
    if cmds.send(Command::GetSettings { reply: reply_tx }).is_err() {
        return http_response("503 Service Unavailable", "text/plain", "writer unavailable");
    }
    match reply_rx.recv() {
        Ok(Ok(levers)) => {
            let body = serde_json::to_string(&levers).unwrap_or_else(|_| "[]".into());
            http_response("200 OK", "application/json", &body)
        }
        Ok(Err(e)) => http_response("409 Conflict", "text/plain", &e),
        Err(_) => http_response("503 Service Unavailable", "text/plain", "no reply from writer"),
    }
}

/// `POST /settings` — overwrite one live lever's value. The writer validates the key + type
/// authoritatively (shape check) before absorbing it.
fn handle_setting(request: &str, cmds: &Sender<Command>) -> String {
    let body = request.split("\r\n\r\n").nth(1).unwrap_or("");
    let dto: wire::SettingCmd = match serde_json::from_str(body) {
        Ok(d) => d,
        Err(e) => return http_response("400 Bad Request", "text/plain", &format!("bad json: {e}")),
    };
    let (reply_tx, reply_rx) = std::sync::mpsc::channel();
    let cmd = Command::SetSetting { key: dto.key, value: dto.value, reply: reply_tx };
    if cmds.send(cmd).is_err() {
        return http_response("503 Service Unavailable", "text/plain", "writer unavailable");
    }
    match reply_rx.recv() {
        Ok(Ok(())) => http_response("200 OK", "text/plain", "ok"),
        Ok(Err(e)) => http_response("409 Conflict", "text/plain", &e),
        Err(_) => http_response("503 Service Unavailable", "text/plain", "no reply from writer"),
    }
}

/// Parse `?iid=N` from a request path.
fn parse_iid(path: &str) -> Option<i64> {
    path.split_once("?iid=")?.1.split('&').next()?.trim().parse().ok()
}

/// Which ticket-targeted hold a request is.
enum Hold {
    Anchor,
    Weigh,
    Sink,
}

/// `POST /anchor` | `/weigh` | `/sink` — parse the ticket command and relay to the writer. Anchor
/// drops the graceful hold; weigh lifts it; sink scuttles the cog (hard teardown, its own verb).
fn handle_hold(request: &str, cmds: &Sender<Command>, hold: Hold) -> String {
    let body = request.split("\r\n\r\n").nth(1).unwrap_or("");
    let dto: wire::CogTarget = match serde_json::from_str(body) {
        Ok(d) => d,
        Err(e) => return http_response("400 Bad Request", "text/plain", &format!("bad json: {e}")),
    };
    // Validate + resolve the cog target (unknown cog / wrong key → a clear 400, never silent).
    let target = match ControlTarget::from_wire(dto) {
        Ok(t) => t,
        Err(e) => return http_response("400 Bad Request", "text/plain", &e),
    };
    let (reply_tx, reply_rx) = std::sync::mpsc::channel();
    let cmd = match hold {
        Hold::Anchor => Command::Anchor { target, reply: reply_tx },
        Hold::Weigh => Command::Weigh { target, reply: reply_tx },
        Hold::Sink => Command::Sink { target, reply: reply_tx },
    };
    if cmds.send(cmd).is_err() {
        return http_response("503 Service Unavailable", "text/plain", "writer unavailable");
    }
    match reply_rx.recv() {
        Ok(Ok(())) => http_response("200 OK", "text/plain", "ok"),
        Ok(Err(e)) => http_response("409 Conflict", "text/plain", &e),
        Err(_) => http_response("503 Service Unavailable", "text/plain", "no reply from writer"),
    }
}

/// `POST /send` — push an operator message into the target cog's tmux pane(s). Validates + resolves
/// the cog target (unknown cog / wrong key → a clear 400), then relays to the writer.
fn handle_send(request: &str, cmds: &Sender<Command>) -> String {
    let body = request.split("\r\n\r\n").nth(1).unwrap_or("");
    let dto: wire::SendMsg = match serde_json::from_str(body) {
        Ok(d) => d,
        Err(e) => return http_response("400 Bad Request", "text/plain", &format!("bad json: {e}")),
    };
    let target = match ControlTarget::from_wire(wire::CogTarget { cog: dto.cog, ticket: dto.ticket, convoy: dto.convoy }) {
        Ok(t) => t,
        Err(e) => return http_response("400 Bad Request", "text/plain", &e),
    };
    let (reply_tx, reply_rx) = std::sync::mpsc::channel();
    if cmds.send(Command::Send { target, message: dto.message, reply: reply_tx }).is_err() {
        return http_response("503 Service Unavailable", "text/plain", "writer unavailable");
    }
    match reply_rx.recv() {
        Ok(Ok(())) => http_response("200 OK", "text/plain", "ok"),
        Ok(Err(e)) => http_response("409 Conflict", "text/plain", &e),
        Err(_) => http_response("503 Service Unavailable", "text/plain", "no reply from writer"),
    }
}

/// `POST /hold` | `/unhold` — set/clear a ticket's `held` flag (keeping it off the dispatch
/// frontier). The route decides `held`; `/hold` carries the groomer's `reason`, `/unhold` ignores it.
fn handle_set_held(request: &str, cmds: &Sender<Command>, held: bool) -> String {
    let body = request.split("\r\n\r\n").nth(1).unwrap_or("");
    let dto: wire::HoldCmd = match serde_json::from_str(body) {
        Ok(d) => d,
        Err(e) => return http_response("400 Bad Request", "text/plain", &format!("bad json: {e}")),
    };
    let ticket = match parse_ticket(&dto.ticket) {
        Ok(t) => t,
        Err(r) => return r,
    };
    let (reply_tx, reply_rx) = std::sync::mpsc::channel();
    let cmd = Command::SetHeld {
        ticket,
        held,
        reason: if held { dto.reason } else { None },
        reply: reply_tx,
    };
    if cmds.send(cmd).is_err() {
        return http_response("503 Service Unavailable", "text/plain", "writer unavailable");
    }
    match reply_rx.recv() {
        Ok(Ok(())) => http_response("200 OK", "text/plain", "ok"),
        Ok(Err(e)) => http_response("409 Conflict", "text/plain", &e),
        Err(_) => http_response("503 Service Unavailable", "text/plain", "no reply from writer"),
    }
}

/// Pull `(method, path)` from the request line, e.g. `POST /status HTTP/1.1`.
fn parse_request_line(request: &str) -> (Option<String>, Option<String>) {
    let mut parts = request.lines().next().unwrap_or("").split_whitespace();
    (parts.next().map(str::to_string), parts.next().map(str::to_string))
}

fn http_response(status: &str, content_type: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\n\
         Content-Type: {content_type}\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\r\n{body}",
        body.len()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[test]
    fn read_request_reads_a_body_that_spans_many_reads() {
        // A reader that hands back 100 bytes at a time, like a socket under a large write.
        struct Trickle(std::io::Cursor<Vec<u8>>);
        impl Read for Trickle {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                let n = buf.len().min(100);
                self.0.read(&mut buf[..n])
            }
        }
        let body = "x".repeat(10_000);
        let raw = format!("POST /launch HTTP/1.1\r\nContent-Length: {}\r\n\r\n{body}", body.len());
        let got = read_request(&mut Trickle(std::io::Cursor::new(raw.clone().into_bytes()))).unwrap();
        assert_eq!(got, raw, "the whole body, not the first buffer");
    }

    #[test]
    fn http_response_has_status_type_and_length() {
        let r = http_response("200 OK", "application/json", "[]");
        assert!(r.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(r.contains("Content-Type: application/json\r\n"));
        assert!(r.contains("Content-Length: 2\r\n"));
        assert!(r.ends_with("\r\n\r\n[]"));
    }

    #[test]
    fn parse_request_line_extracts_method_and_path() {
        let (m, p) = parse_request_line("POST /status HTTP/1.1\r\nHost: x\r\n\r\n");
        assert_eq!((m.as_deref(), p.as_deref()), (Some("POST"), Some("/status")));
        assert_eq!(parse_request_line(""), (None, None));
    }

    /// Spawn `serve` on a temp socket, waiting for the file to appear. Returns the path.
    fn spawn_server(snapshot: Snapshot, cmds: Sender<Command>, tag: &str) -> String {
        let sock = format!("/tmp/fleetd-test-{}-{tag}.sock", std::process::id());
        let (sock2, stacks) = (sock.clone(), tag != "nostack");
        std::thread::spawn(move || {
            let deployment =
                Deployment { project: "/wt/project".into(), default_branch: "origin/main".into(), stacks };
            let _ = serve(&sock2, snapshot, cmds, deployment);
        });
        for _ in 0..100 {
            if std::path::Path::new(&sock).exists() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        sock
    }

    #[test]
    fn a_live_socket_refuses_a_second_fleetd_but_a_stale_file_does_not() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let sock = spawn_server(Arc::new(Mutex::new(vec![])), tx, "guard");
        assert!(refuse_if_running(&sock).unwrap_err().to_string().contains("already running"));
        let stale = format!("/tmp/fleetd-test-{}-stale.sock", std::process::id());
        // A leftover file nobody answers on. (Not a bound-then-dropped listener: on macOS a process
        // another test spawns mid-bind can inherit the socket and keep it answering.)
        std::fs::write(&stale, b"").unwrap();
        assert!(refuse_if_running(&stale).is_ok());
        let _ = std::fs::remove_file(&stale);
    }

    fn roundtrip(sock: &str, req: &[u8]) -> String {
        let mut c = UnixStream::connect(sock).expect("connect");
        c.write_all(req).unwrap();
        let mut resp = String::new();
        c.read_to_string(&mut resp).unwrap();
        resp
    }

    #[test]
    fn serves_context_and_404s_unknown_paths() {
        let snapshot: Snapshot = Arc::new(Mutex::new(Vec::new())); // empty → "[]"
        let (tx, _rx) = std::sync::mpsc::channel();
        let sock = spawn_server(snapshot, tx, "ctx");

        let resp = roundtrip(&sock, b"GET /context HTTP/1.1\r\n\r\n");
        assert!(resp.contains("200 OK"), "resp: {resp}");
        assert!(resp.contains("application/json"));

        // the project checkout path, served from startup config (no writer round-trip)
        let proj = roundtrip(&sock, b"GET /project HTTP/1.1\r\n\r\n");
        assert!(proj.contains("200 OK") && proj.ends_with("\r\n\r\n/wt/project"), "resp: {proj}");
        assert!(resp.trim_end().ends_with("[]"));

        let resp = roundtrip(&sock, b"GET /nope HTTP/1.1\r\n\r\n");
        assert!(resp.contains("404 Not Found"), "resp: {resp}");

        let _ = std::fs::remove_file(&sock);
    }

    #[test]
    fn post_status_validates_then_relays_to_the_writer() {
        let snapshot: Snapshot = Arc::new(Mutex::new(Vec::new()));
        let (tx, rx) = std::sync::mpsc::channel::<Command>();
        // Fake writer: accept any RecordStatus and reply Ok.
        std::thread::spawn(move || {
            for cmd in rx {
                match cmd {
                    Command::RecordStatus { reply, .. }
                    | Command::Dispatch { reply, .. }
                    | Command::LaunchHand { reply, .. }
                    | Command::OpenConvoy { reply, .. }
                    | Command::CloseConvoy { reply, .. }
                    | Command::Anchor { reply, .. }
                    | Command::Weigh { reply, .. }
                    | Command::Sink { reply, .. }
                    | Command::Send { reply, .. }
                    | Command::SetHeld { reply, .. }
                    | Command::SetSetting { reply, .. }
                    | Command::Rebase { reply, .. } => { let _ = reply.send(Ok(())); }
                    Command::GetPacket { reply, .. } => { let _ = reply.send(Err("n/a".into())); }
                    Command::GetConvoyView { reply, .. } => { let _ = reply.send(Err("n/a".into())); }
                    Command::GetReview { reply, .. } => { let _ = reply.send(Err("n/a".into())); }
                    Command::GetSettings { reply, .. } => { let _ = reply.send(Err("n/a".into())); }
                    Command::Launch { reply, .. } => { let _ = reply.send(Err("n/a".into())); }
                    Command::GetTicket { reply, .. } => { let _ = reply.send(Err("n/a".into())); }
                    Command::NewTicket { reply, .. } => { let _ = reply.send(Err("n/a".into())); }
                    Command::Propose { reply, .. }
                    | Command::AcceptProposal { reply, .. }
                    | Command::AddToConvoy { reply, .. }
                    | Command::Decommission { reply, .. }
                    | Command::RemoveFromConvoy { reply, .. }
                    | Command::DropProposal { reply, .. } => { let _ = reply.send(Err("n/a".into())); }
                    Command::BlockTicket { reply, .. }
                    | Command::EditTicket { reply, .. }
                    | Command::CommentTicket { reply, .. } => { let _ = reply.send(Ok(())); }
                    Command::Arm { reply, .. } => { let _ = reply.send(Ok(())); }
                    Command::GetCi { reply, .. } => { let _ = reply.send(Err("n/a".into())); }
                    Command::Reply { reply, .. } => { let _ = reply.send(Ok(())); }
                    Command::GetShip { reply, .. } => { let _ = reply.send(Err("n/a".into())); }
                    Command::PostReview { reply, .. } => { let _ = reply.send(Ok(())); }
                    Command::RetryCi { reply, .. } => { let _ = reply.send(Ok(())); }
                }
            }
        });
        let sock = spawn_server(snapshot, tx, "status");

        // A valid report is relayed and acked.
        let body = r#"{"cog":"worker","ticket":"PROJ-1","state":"concluded","last_action":null}"#;
        let req = format!("POST /status HTTP/1.1\r\nContent-Length: {}\r\n\r\n{body}", body.len());
        assert!(roundtrip(&sock, req.as_bytes()).contains("200 OK"));

        // An invalid state for the cog is rejected at the edge (never reaches the writer).
        let bad = r#"{"cog":"worker","ticket":"PROJ-1","state":"finishing","last_action":null}"#;
        let req = format!("POST /status HTTP/1.1\r\nContent-Length: {}\r\n\r\n{bad}", bad.len());
        assert!(roundtrip(&sock, req.as_bytes()).contains("400 Bad Request"));

        let _ = std::fs::remove_file(&sock);
    }

    #[test]
    fn post_dispatch_relays_the_order_to_the_writer() {
        let snapshot: Snapshot = Arc::new(Mutex::new(Vec::new()));
        let (tx, rx) = std::sync::mpsc::channel::<Command>();
        let (seen_tx, seen) = std::sync::mpsc::channel::<String>(); // each relayed order's fork_from
        std::thread::spawn(move || {
            for cmd in rx {
                match cmd {
                    Command::Dispatch { order, reply } => {
                        let _ = seen_tx.send(order.fork_from);
                        let _ = reply.send(Ok(()));
                    }
                    Command::RecordStatus { reply, .. }
                    | Command::LaunchHand { reply, .. }
                    | Command::OpenConvoy { reply, .. }
                    | Command::CloseConvoy { reply, .. }
                    | Command::Anchor { reply, .. }
                    | Command::Weigh { reply, .. }
                    | Command::Sink { reply, .. }
                    | Command::Send { reply, .. }
                    | Command::SetHeld { reply, .. }
                    | Command::SetSetting { reply, .. }
                    | Command::Rebase { reply, .. } => { let _ = reply.send(Ok(())); }
                    Command::GetPacket { reply, .. } => { let _ = reply.send(Err("n/a".into())); }
                    Command::GetConvoyView { reply, .. } => { let _ = reply.send(Err("n/a".into())); }
                    Command::GetReview { reply, .. } => { let _ = reply.send(Err("n/a".into())); }
                    Command::GetSettings { reply, .. } => { let _ = reply.send(Err("n/a".into())); }
                    Command::Launch { reply, .. } => { let _ = reply.send(Err("n/a".into())); }
                    Command::GetTicket { reply, .. } => { let _ = reply.send(Err("n/a".into())); }
                    Command::NewTicket { reply, .. } => { let _ = reply.send(Err("n/a".into())); }
                    Command::Propose { reply, .. }
                    | Command::AcceptProposal { reply, .. }
                    | Command::AddToConvoy { reply, .. }
                    | Command::Decommission { reply, .. }
                    | Command::RemoveFromConvoy { reply, .. }
                    | Command::DropProposal { reply, .. } => { let _ = reply.send(Err("n/a".into())); }
                    Command::BlockTicket { reply, .. }
                    | Command::EditTicket { reply, .. }
                    | Command::CommentTicket { reply, .. } => { let _ = reply.send(Ok(())); }
                    Command::Arm { reply, .. } => { let _ = reply.send(Ok(())); }
                    Command::GetCi { reply, .. } => { let _ = reply.send(Err("n/a".into())); }
                    Command::Reply { reply, .. } => { let _ = reply.send(Ok(())); }
                    Command::GetShip { reply, .. } => { let _ = reply.send(Err("n/a".into())); }
                    Command::PostReview { reply, .. } => { let _ = reply.send(Ok(())); }
                    Command::RetryCi { reply, .. } => { let _ = reply.send(Ok(())); }
                }
            }
        });
        let sock = spawn_server(snapshot, tx, "dispatch");

        let body = r#"{"ticket":"PROJ-1","auto_impl":true,"fork_from":"origin/master","ac_summary":"do it","park_at":null}"#;
        let req = format!("POST /dispatch HTTP/1.1\r\nContent-Length: {}\r\n\r\n{body}", body.len());
        assert!(roundtrip(&sock, req.as_bytes()).contains("200 OK"));
        assert_eq!(seen.recv().unwrap(), "origin/master", "an explicit fork is kept");

        // No fork_from on the wire → the deployment's default branch.
        let body = r#"{"ticket":"PROJ-1","auto_impl":true,"ac_summary":"do it","park_at":null}"#;
        let req = format!("POST /dispatch HTTP/1.1\r\nContent-Length: {}\r\n\r\n{body}", body.len());
        assert!(roundtrip(&sock, req.as_bytes()).contains("200 OK"));
        assert_eq!(seen.recv().unwrap(), "origin/main", "an unset fork inherits the deployment's");

        // Malformed JSON is rejected at the edge.
        let req = "POST /dispatch HTTP/1.1\r\nContent-Length: 3\r\n\r\n{ x";
        assert!(roundtrip(&sock, req.as_bytes()).contains("400 Bad Request"));

        let _ = std::fs::remove_file(&sock);
    }

    #[test]
    fn post_propose_relays_the_draft_and_rejects_a_bad_key_at_the_edge() {
        let (tx, rx) = std::sync::mpsc::channel::<Command>();
        std::thread::spawn(move || {
            for cmd in rx {
                if let Command::Propose { from, proposed, reply } = cmd {
                    let what = match proposed {
                        domain::Proposed::New(d) => d.title,
                        domain::Proposed::Existing { ticket, .. } => ticket.0,
                    };
                    let _ = reply.send(Ok(format!("{}:{what}", from.map(|k| k.0).unwrap_or_default())));
                }
            }
        });
        let sock = spawn_server(Arc::new(Mutex::new(Vec::new())), tx, "propose");
        let post = |body: &str| {
            let req = format!("POST /propose HTTP/1.1\r\nContent-Length: {}\r\n\r\n{body}", body.len());
            roundtrip(&sock, req.as_bytes())
        };
        let ok = post(r#"{"from_ticket":"PROJ-1","proposed":{"new":{"title":"Fix X","kind":"bug","description":"…"}}}"#);
        assert!(ok.contains("200 OK") && ok.ends_with("PROJ-1:Fix X"), "resp: {ok}");
        let ok = post(r#"{"from_ticket":"PROJ-1","proposed":{"existing":{"ticket":"PROJ-7","why":"blocks the merge"}}}"#);
        assert!(ok.contains("200 OK") && ok.ends_with("PROJ-1:PROJ-7"), "resp: {ok}");
        assert!(post(r#"{"from_ticket":"nope","proposed":{"new":{"title":"Fix X","kind":"bug","description":"…"}}}"#).contains("400"));
        assert!(post(r#"{"proposed":{"new":{"title":" ","kind":"bug","description":"…"}}}"#).contains("400"));
        assert!(post(r#"{"proposed":{"existing":{"ticket":"PROJ-7","why":" "}}}"#).contains("400"));
        let _ = std::fs::remove_file(&sock);
    }

    #[test]
    fn a_forge_that_cant_stack_rejects_a_non_default_fork() {
        let snapshot: Snapshot = Arc::new(Mutex::new(Vec::new()));
        let (tx, rx) = std::sync::mpsc::channel::<Command>();
        std::thread::spawn(move || {
            for cmd in rx {
                if let Command::Dispatch { reply, .. } = cmd {
                    let _ = reply.send(Ok(()));
                }
            }
        });
        let sock = spawn_server(snapshot, tx, "nostack");
        let post = |fork: &str| {
            let body = format!(r#"{{"ticket":"PROJ-2","auto_impl":true,{fork}"ac_summary":"do it","park_at":null}}"#);
            roundtrip(&sock, format!("POST /dispatch HTTP/1.1\r\nContent-Length: {}\r\n\r\n{body}", body.len()).as_bytes())
        };
        let stacked = post(r#""fork_from":"origin/chore/PROJ-1-parent","#);
        assert!(stacked.contains("400 Bad Request") && stacked.contains("can't stack"), "resp: {stacked}");
        assert!(post(r#""fork_from":"main","#).contains("200 OK"), "the default branch, origin/ or not");
        assert!(post("").contains("200 OK"), "no fork_from → the default branch");
        let _ = std::fs::remove_file(&sock);
    }

}
