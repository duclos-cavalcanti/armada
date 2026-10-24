//! OTLP/JSON → `TelemetryRow` parsing (logs-first). A Rust re-iteration of caprock's `_attrs()`
//! flatten over the OTLP logs structure: `resourceLogs → resource.attributes → scopeLogs →
//! logRecords[]`. Hot keys (`cost_usd`, `model`, `session.id`) are promoted to columns; the
//! remainder lands in `attrs_json`. Tolerant of shape drift — never panics on a missing/odd field.

use ledger::telemetry::TelemetryRow;
use serde_json::{Map, Value};

/// Parse an OTLP/HTTP logs export (JSON) into telemetry rows — one per log record. `received_at`
/// is the caller's ingest timestamp.
pub fn parse_logs(body: &Value, received_at: &str) -> Vec<TelemetryRow> {
    let mut rows = Vec::new();
    for rl in array(body, "resourceLogs") {
        // Resource-level attributes: service.name + our attribution (fleet.role / fleet.ticket).
        let res = attrs_map(rl.pointer("/resource/attributes"));
        let service = res.get("service.name").and_then(as_string);
        let role = res.get("fleet.role").and_then(as_string);
        let ticket = res.get("fleet.ticket").and_then(as_string);
        let res_session = res.get("session.id").and_then(as_string);

        for sl in array(rl, "scopeLogs") {
            for rec in array(sl, "logRecords") {
                let event = rec
                    .pointer("/body/stringValue")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let ts = rec.get("timeUnixNano").and_then(unix_nano_to_rfc3339);

                // Record-level attributes: promote the hot keys, leave the rest in attrs_json.
                let mut attrs = attrs_map(rec.get("attributes"));
                let cost_usd = attrs.remove("cost_usd").as_ref().and_then(Value::as_f64);
                let model = attrs.remove("model").and_then(|v| as_string(&v));
                let rec_session = attrs.remove("session.id").and_then(|v| as_string(&v));

                rows.push(TelemetryRow {
                    received_at: received_at.to_string(),
                    ts,
                    event,
                    service: service.clone(),
                    role: role.clone(),
                    ticket: ticket.clone(),
                    session_id: res_session.clone().or(rec_session),
                    cost_usd,
                    model,
                    attrs_json: serde_json::to_string(&attrs).unwrap_or_else(|_| "{}".into()),
                });
            }
        }
    }
    rows
}

/// An OTLP attribute list `[{key, value:{<typed>}}]` flattened to a `key → unwrapped Value` map
/// (the caprock `_attrs()` move). Missing/None → empty map.
fn attrs_map(attrs: Option<&Value>) -> Map<String, Value> {
    let mut out = Map::new();
    for a in attrs.and_then(Value::as_array).into_iter().flatten() {
        let Some(key) = a.get("key").and_then(Value::as_str) else { continue };
        if let Some(val) = a.get("value").and_then(unwrap_value) {
            out.insert(key.to_string(), val);
        }
    }
    out
}

/// Unwrap an OTLP typed value `{stringValue|doubleValue|intValue|boolValue}` to a JSON scalar.
/// `intValue` is string-encoded per OTLP/JSON → normalize to a number when parseable.
fn unwrap_value(v: &Value) -> Option<Value> {
    if let Some(s) = v.get("stringValue") {
        return Some(s.clone());
    }
    if let Some(d) = v.get("doubleValue") {
        return Some(d.clone());
    }
    if let Some(i) = v.get("intValue") {
        return Some(
            i.as_str()
                .and_then(|s| s.parse::<i64>().ok())
                .map(Value::from)
                .unwrap_or_else(|| i.clone()),
        );
    }
    if let Some(b) = v.get("boolValue") {
        return Some(b.clone());
    }
    None
}

fn array<'a>(v: &'a Value, key: &str) -> impl Iterator<Item = &'a Value> {
    v.get(key).and_then(Value::as_array).into_iter().flatten()
}

fn as_string(v: &Value) -> Option<String> {
    v.as_str().map(String::from)
}

/// `timeUnixNano` (a string of nanoseconds since the epoch) → RFC3339, or None if unparseable.
fn unix_nano_to_rfc3339(v: &Value) -> Option<String> {
    let nanos: i128 = v.as_str()?.parse().ok()?;
    time::OffsetDateTime::from_unix_timestamp_nanos(nanos)
        .ok()?
        .format(&time::format_description::well_known::Rfc3339)
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_an_api_request_log_into_a_row() {
        let body = json!({
            "resourceLogs": [{
                "resource": { "attributes": [
                    {"key":"service.name","value":{"stringValue":"fleet-worker"}},
                    {"key":"fleet.role","value":{"stringValue":"worker"}},
                    {"key":"fleet.ticket","value":{"stringValue":"PROJ-1"}},
                    {"key":"session.id","value":{"stringValue":"sess-1"}}
                ]},
                "scopeLogs": [{ "logRecords": [{
                    "timeUnixNano": "1780000000000000000",
                    "body": {"stringValue":"claude_code.api_request"},
                    "attributes": [
                        {"key":"cost_usd","value":{"doubleValue":1.5}},
                        {"key":"model","value":{"stringValue":"claude-opus"}},
                        {"key":"output_tokens","value":{"intValue":"100"}}
                    ]
                }]}]
            }]
        });

        let rows = parse_logs(&body, "2026-10-13T00:00:00Z");
        assert_eq!(rows.len(), 1);
        let r = &rows[0];
        assert_eq!(r.event, "claude_code.api_request");
        assert_eq!(r.service.as_deref(), Some("fleet-worker"));
        assert_eq!(r.role.as_deref(), Some("worker"));
        assert_eq!(r.ticket.as_deref(), Some("PROJ-1"));
        assert_eq!(r.session_id.as_deref(), Some("sess-1"));
        assert_eq!(r.cost_usd, Some(1.5));
        assert_eq!(r.model.as_deref(), Some("claude-opus"));
        assert!(r.ts.as_deref().unwrap().starts_with("2026-"));

        // attrs_json = the remainder: promoted keys removed, output_tokens kept (as a number).
        let attrs: Value = serde_json::from_str(&r.attrs_json).unwrap();
        assert_eq!(attrs["output_tokens"], 100);
        assert!(attrs.get("cost_usd").is_none(), "promoted keys excluded from attrs_json");
        assert!(attrs.get("model").is_none());
    }

    #[test]
    fn a_one_off_session_has_no_role_or_ticket() {
        let body = json!({
            "resourceLogs": [{
                "resource": { "attributes": [
                    {"key":"service.name","value":{"stringValue":"claude-code"}}
                ]},
                "scopeLogs": [{ "logRecords": [{
                    "body": {"stringValue":"claude_code.api_request"},
                    "attributes": [{"key":"cost_usd","value":{"doubleValue":2.0}}]
                }]}]
            }]
        });
        let rows = parse_logs(&body, "r");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].role, None, "a one-off → NULL role (the interactive bucket)");
        assert_eq!(rows[0].ticket, None);
        assert_eq!(rows[0].cost_usd, Some(2.0));
    }

    #[test]
    fn tolerates_empty_or_odd_shapes() {
        assert!(parse_logs(&json!({}), "r").is_empty());
        assert!(parse_logs(&json!({"resourceLogs": []}), "r").is_empty());
        assert!(parse_logs(&json!({"resourceLogs": [{}]}), "r").is_empty());
    }
}
