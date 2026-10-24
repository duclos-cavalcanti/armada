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

/// Bind the socket (clearing any stale file) and serve forever. `cmds` is the write
/// channel to the scheduler; each connection gets its own clone.
pub fn serve(path: &str, snapshot: Snapshot, cmds: Sender<Command>) -> Result<()> {
    let _ = std::fs::remove_file(path); // a leftover socket from a crash would block bind
    let listener = UnixListener::bind(path)?;
    for stream in listener.incoming() {
        match stream {
            Ok(mut s) => {
                let snapshot = snapshot.clone();
                let cmds = cmds.clone();
                std::thread::spawn(move || {
                    if let Err(e) = handle(&mut s, &snapshot, &cmds) {
                        eprintln!("fleetd: connection error: {e}");
                    }
                });
            }
            Err(e) => eprintln!("fleetd: accept error: {e}"),
        }
    }
    Ok(())
}

fn handle(stream: &mut UnixStream, snapshot: &Snapshot, cmds: &Sender<Command>) -> std::io::Result<()> {
    let mut buf = [0u8; 2048];
    let n = stream.read(&mut buf)?;
    let request = String::from_utf8_lossy(&buf[..n]);
    let (method, path) = parse_request_line(&request);

    let response = match (method.as_deref(), path.as_deref()) {
        (Some("GET"), Some(p)) if p.starts_with("/context?convoy=") => handle_convoy_view(cmds, p),
        (Some("GET"), Some("/context")) => {
            let body = serde_json::to_string(&*snapshot.lock().unwrap())
                .unwrap_or_else(|_| "[]".into());
            http_response("200 OK", "application/json", &body)
        }
        (Some("GET"), Some("/health")) => http_response("200 OK", "text/plain", "ok"),
        (Some("POST"), Some("/status")) => handle_status(&request, cmds),
        (Some("POST"), Some("/dispatch")) => handle_dispatch(&request, cmds),
        (Some("POST"), Some("/hand")) => handle_hand(&request, cmds),
        (Some("POST"), Some("/convoy-open")) => handle_convoy(&request, cmds, true),
        (Some("POST"), Some("/convoy-close")) => handle_convoy(&request, cmds, false),
        (Some("POST"), Some("/anchor")) => handle_hold(&request, cmds, Hold::Anchor),
        (Some("POST"), Some("/weigh")) => handle_hold(&request, cmds, Hold::Weigh),
        (Some("POST"), Some("/sink")) => handle_hold(&request, cmds, Hold::Sink),
        (Some("POST"), Some("/send")) => handle_send(&request, cmds),
        (Some("POST"), Some("/hold")) => handle_set_held(&request, cmds, true),
        (Some("POST"), Some("/unhold")) => handle_set_held(&request, cmds, false),
        (Some("GET"), Some(p)) if p.starts_with("/packet") => handle_packet(cmds, p),
        (Some("GET"), Some(p)) if p.starts_with("/review") => handle_review(cmds, p),
        (Some("POST"), Some(p)) if p.starts_with("/rebase") => handle_rebase(cmds, p),
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

    let ticket = match parse_ticket(&dto.ticket) {
        Ok(t) => t,
        Err(r) => return r,
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
fn parse_ticket(s: &str) -> Result<TicketKey, String> {
    TicketKey::parse(s).map_err(|e| http_response("400 Bad Request", "text/plain", &e.to_string()))
}

/// `POST /dispatch` — parse the order packet, stamp `created_at`, relay to the writer.
/// The ledger's FK (ticket must exist on the board) is the authoritative check.
fn handle_dispatch(request: &str, cmds: &Sender<Command>) -> String {
    let body = request.split("\r\n\r\n").nth(1).unwrap_or("");
    let dto: wire::DispatchOrder = match serde_json::from_str(body) {
        Ok(d) => d,
        Err(e) => return http_response("400 Bad Request", "text/plain", &format!("bad json: {e}")),
    };
    let ticket = match parse_ticket(&dto.ticket) {
        Ok(t) => t,
        Err(r) => return r,
    };
    let order = Order {
        ticket,
        auto_impl: dto.auto_impl,
        auto_review: dto.auto_review,
        fork_from: dto.fork_from,
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
    let key = ConvoyKey(dto.key);
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
    let (reply_tx, reply_rx) = std::sync::mpsc::channel();
    let cmd = Command::GetConvoyView { convoy: ConvoyKey(convoy.to_string()), reply: reply_tx };
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
        let sock2 = sock.clone();
        std::thread::spawn(move || {
            let _ = serve(&sock2, snapshot, cmds);
        });
        for _ in 0..100 {
            if std::path::Path::new(&sock).exists() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        sock
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
                }
            }
        });
        let sock = spawn_server(snapshot, tx, "status");

        // A valid report is relayed and acked.
        let body = r#"{"cog":"worker","ticket":"MADY-1","state":"concluded","last_action":null}"#;
        let req = format!("POST /status HTTP/1.1\r\nContent-Length: {}\r\n\r\n{body}", body.len());
        assert!(roundtrip(&sock, req.as_bytes()).contains("200 OK"));

        // An invalid state for the cog is rejected at the edge (never reaches the writer).
        let bad = r#"{"cog":"worker","ticket":"MADY-1","state":"finishing","last_action":null}"#;
        let req = format!("POST /status HTTP/1.1\r\nContent-Length: {}\r\n\r\n{bad}", bad.len());
        assert!(roundtrip(&sock, req.as_bytes()).contains("400 Bad Request"));

        let _ = std::fs::remove_file(&sock);
    }

    #[test]
    fn post_dispatch_relays_the_order_to_the_writer() {
        let snapshot: Snapshot = Arc::new(Mutex::new(Vec::new()));
        let (tx, rx) = std::sync::mpsc::channel::<Command>();
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
                }
            }
        });
        let sock = spawn_server(snapshot, tx, "dispatch");

        let body = r#"{"ticket":"MADY-1","auto_impl":true,"auto_review":false,"fork_from":"origin/master","ac_summary":"do it","park_at":null}"#;
        let req = format!("POST /dispatch HTTP/1.1\r\nContent-Length: {}\r\n\r\n{body}", body.len());
        assert!(roundtrip(&sock, req.as_bytes()).contains("200 OK"));

        // Malformed JSON is rejected at the edge.
        let req = "POST /dispatch HTTP/1.1\r\nContent-Length: 3\r\n\r\n{ x";
        assert!(roundtrip(&sock, req.as_bytes()).contains("400 Bad Request"));

        let _ = std::fs::remove_file(&sock);
    }
}
