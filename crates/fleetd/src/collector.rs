//! The OTLP/HTTP telemetry receiver (T3) — fleetd's built-in collector, replacing the external
//! otelcol. Binds a TCP port that cogs POST their OpenTelemetry to, parses the logs (otlp.rs),
//! and writes rows to the telemetry store. Separate from the UDS context server: cogs push to a
//! host:port URL, and telemetry lands in telemetry.db, never the ledger. One thread per
//! connection + a Mutex'd store (its own connection) is plenty for a local collector.

use crate::otlp;
use anyhow::Result;
use ledger::telemetry::TelemetryStore;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

/// Bind `addr` and serve OTLP/HTTP forever, writing rows to `store`.
pub fn serve(addr: &str, store: Arc<Mutex<TelemetryStore>>) -> Result<()> {
    serve_on(TcpListener::bind(addr)?, store)
}

/// The accept loop on a pre-bound listener — split from `serve` so a test can bind an ephemeral
/// port (`127.0.0.1:0`) and learn the address before serving.
fn serve_on(listener: TcpListener, store: Arc<Mutex<TelemetryStore>>) -> Result<()> {
    for stream in listener.incoming() {
        match stream {
            Ok(mut s) => {
                let store = store.clone();
                std::thread::spawn(move || {
                    if let Err(e) = handle(&mut s, &store) {
                        eprintln!("fleetd: otlp receiver error: {e}");
                    }
                });
            }
            Err(e) => eprintln!("fleetd: otlp accept error: {e}"),
        }
    }
    Ok(())
}

fn handle(stream: &mut TcpStream, store: &Arc<Mutex<TelemetryStore>>) -> std::io::Result<()> {
    let (path, body) = read_request(stream)?;
    let response = match path.as_deref() {
        Some("/v1/logs") => ingest_logs(&body, &store.lock().unwrap()),
        // Metrics / traces accepted + ignored: the per-request cost lives on the log events.
        Some("/v1/metrics") | Some("/v1/traces") => ok(),
        _ => http("404 Not Found", "not found"),
    };
    stream.write_all(response.as_bytes())
}

/// Parse an OTLP logs body and append each record to the store. Returns the HTTP response —
/// `200` with an empty `ExportLogsServiceResponse` on success, `400` on bad JSON. A per-row write
/// failure is logged, not fatal (the rest still land).
fn ingest_logs(body: &str, store: &TelemetryStore) -> String {
    let json: serde_json::Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(e) => return http("400 Bad Request", &format!("bad json: {e}")),
    };
    for row in otlp::parse_logs(&json, &crate::now_rfc3339()) {
        if let Err(e) = store.record(&row) {
            eprintln!("fleetd: telemetry record failed: {e}");
        }
    }
    ok()
}

/// Read a full HTTP request: the head, then exactly `Content-Length` body bytes — an OTLP batch
/// can exceed a single read, so we can't rely on one `read()`. Returns `(path, body)`.
fn read_request(stream: &mut TcpStream) -> std::io::Result<(Option<String>, String)> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        if let Some(pos) = find_subslice(&buf, b"\r\n\r\n") {
            break pos + 4;
        }
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            return Ok((None, String::new())); // closed before a full header
        }
        buf.extend_from_slice(&chunk[..n]);
    };
    let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
    let path = head.lines().next().and_then(|l| l.split_whitespace().nth(1)).map(str::to_string);
    let want = content_length(&head);
    while buf.len() - header_end < want {
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let body = String::from_utf8_lossy(&buf[header_end..]).to_string();
    Ok((path, body))
}

fn content_length(head: &str) -> usize {
    head.lines()
        .find_map(|l| l.split_once(':').filter(|(k, _)| k.trim().eq_ignore_ascii_case("content-length")))
        .and_then(|(_, v)| v.trim().parse().ok())
        .unwrap_or(0)
}

fn find_subslice(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn ok() -> String {
    http("200 OK", "{}")
}

fn http(status: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{"resourceLogs":[{"resource":{"attributes":[
        {"key":"fleet.role","value":{"stringValue":"worker"}}]},
        "scopeLogs":[{"logRecords":[{"timeUnixNano":"1780000000000000000",
        "body":{"stringValue":"claude_code.api_request"},
        "attributes":[{"key":"cost_usd","value":{"doubleValue":0.5}}]}]}]}]}"#;

    #[test]
    fn ingest_logs_writes_rows_and_returns_200() {
        let store = TelemetryStore::in_memory().unwrap();
        assert!(ingest_logs(SAMPLE, &store).contains("200 OK"));
        let got = store.cost_by_role("2026-01-01T00:00:00Z").unwrap();
        assert_eq!(got, vec![("worker".to_string(), 0.5)]);
    }

    #[test]
    fn ingest_logs_rejects_bad_json() {
        let store = TelemetryStore::in_memory().unwrap();
        assert!(ingest_logs("{ not json", &store).contains("400 Bad Request"));
    }

    #[test]
    fn content_length_is_case_insensitive_and_defaults_to_zero() {
        assert_eq!(content_length("POST / HTTP/1.1\r\ncontent-length: 42\r\n\r\n"), 42);
        assert_eq!(content_length("POST / HTTP/1.1\r\nContent-Length: 7\r\n\r\n"), 7);
        assert_eq!(content_length("POST / HTTP/1.1\r\n\r\n"), 0);
    }

    #[test]
    fn read_request_assembles_a_body_larger_than_one_read() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let body = "x".repeat(5000); // > one 4096 chunk → forces a multi-read assembly
        let req = format!("POST /v1/logs HTTP/1.1\r\nContent-Length: {}\r\n\r\n{body}", body.len());
        let t = std::thread::spawn(move || {
            let mut c = TcpStream::connect(addr).unwrap();
            c.write_all(req.as_bytes()).unwrap();
        });
        let (mut s, _) = listener.accept().unwrap();
        let (path, got) = read_request(&mut s).unwrap();
        assert_eq!(path.as_deref(), Some("/v1/logs"));
        assert_eq!(got.len(), 5000);
        t.join().unwrap();
    }

    #[test]
    fn live_post_to_the_receiver_lands_a_row() {
        // End-to-end over a real socket: bind ephemeral → serve_on → POST an OTLP body →
        // assert 200 and that the row is in the store. Closes the serve/accept seam.
        let store = Arc::new(Mutex::new(TelemetryStore::in_memory().unwrap()));
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let srv_store = store.clone();
        std::thread::spawn(move || {
            let _ = serve_on(listener, srv_store);
        });

        let req = format!("POST /v1/logs HTTP/1.1\r\nContent-Length: {}\r\n\r\n{SAMPLE}", SAMPLE.len());
        let mut c = TcpStream::connect(addr).unwrap();
        c.write_all(req.as_bytes()).unwrap();
        let mut resp = String::new();
        c.read_to_string(&mut resp).unwrap();
        assert!(resp.contains("200 OK"), "resp: {resp}");

        let got = store.lock().unwrap().cost_by_role("2026-01-01T00:00:00Z").unwrap();
        assert_eq!(got, vec![("worker".to_string(), 0.5)]);
    }
}
