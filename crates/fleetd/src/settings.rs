//! The live-config overlay — fleetd's runtime-patchable levers (PLAN §config; #23).
//!
//! `config.rs` is the STARTUP base (defaults < toml < env); this module is the LIVE layer
//! on top, backed by the ledger's `settings` KV. The full precedence is
//! **defaults < toml < settings < env**, encoded in ONE place: the `resolve_*` primitives.
//! Any part of the factory reads a live lever through them — a future string lever (e.g. a
//! `fork_strategy` in `reconcile`) adds a `resolve_str` mirroring these, no new plumbing. The
//! named accessors below (`poll_secs`, `dry_run`, …) are thin typed wrappers over the primitive
//! for the levers that mirror `fleet.toml`; the [`LEVERS`] registry drives `settings set`
//! validation and the `list` report.

use crate::config::{self, Config};
use domain::LivenessThresholds;
use ledger::Ledger;
use serde::Serialize;

// ── the precedence primitive (base < setting < env), one encoding, reused everywhere ──

/// Resolve a `u64` lever (the cadences): `env` override (if its var is set) > ledger `setting` >
/// `base`. `env` is the override var name, or `None` for a setting-only lever. An unparseable or
/// missing layer falls through (a live read must never panic the loop). A string lever adds a
/// `resolve_str` mirroring this shape.
pub fn resolve_u64(ledger: &Ledger, env: Option<&str>, key: &str, base: u64) -> u64 {
    env.and_then(config::env_num)
        .or_else(|| setting(ledger, key).and_then(|v| v.parse().ok()))
        .unwrap_or(base)
}

/// Resolve an `i64` lever (the liveness thresholds). Same precedence.
pub fn resolve_i64(ledger: &Ledger, env: Option<&str>, key: &str, base: i64) -> i64 {
    env.and_then(config::env_num)
        .map(|v| v as i64)
        .or_else(|| setting(ledger, key).and_then(|v| v.parse().ok()))
        .unwrap_or(base)
}

/// Resolve a `bool` lever (`dry_run`). Same precedence; env + setting share `parse_bool` spellings.
pub fn resolve_bool(ledger: &Ledger, env: Option<&str>, key: &str, base: bool) -> bool {
    env.and_then(config::env_bool)
        .or_else(|| setting(ledger, key).and_then(|v| config::parse_bool(&v)))
        .unwrap_or(base)
}

/// Best-effort live read of one key (a DB error = absent → fall through).
fn setting(ledger: &Ledger, key: &str) -> Option<String> {
    ledger.setting_get(key).ok().flatten()
}

// ── the fleet.toml-mirrored levers: typed wrappers over the primitive (base = cfg.<field>) ──

pub fn poll_secs(cfg: &Config, l: &Ledger) -> u64 {
    resolve_u64(l, Some("FLEET_POLL_SECS"), "poll_secs", cfg.poll_secs)
}
pub fn sync_secs(cfg: &Config, l: &Ledger) -> u64 {
    resolve_u64(l, Some("FLEET_SYNC_SECS"), "sync_secs", cfg.sync_secs)
}
pub fn reconcile_secs(cfg: &Config, l: &Ledger) -> u64 {
    resolve_u64(l, Some("FLEET_RECONCILE_SECS"), "reconcile_secs", cfg.reconcile_secs)
}
pub fn dry_run(cfg: &Config, l: &Ledger) -> bool {
    resolve_bool(l, Some("FLEET_DRY_RUN"), "dry_run", cfg.dry_run)
}
pub fn thresholds(cfg: &Config, l: &Ledger) -> LivenessThresholds {
    LivenessThresholds {
        active_secs: resolve_i64(l, Some("FLEET_ACTIVE_SECS"), "active_secs", cfg.active_secs),
        quiet_secs: resolve_i64(l, Some("FLEET_QUIET_SECS"), "quiet_secs", cfg.quiet_secs),
    }
}

// ── the registry: the known, operator-tunable levers (drives validate + list) ──

/// A lever's value type — the one spot that knows `u64` vs `i64` vs `bool`, for `set` validation.
#[derive(Clone, Copy)]
enum Ty {
    U64,
    I64,
    Bool,
}

impl Ty {
    fn accepts(self, value: &str) -> bool {
        match self {
            Ty::U64 => value.parse::<u64>().is_ok(),
            Ty::I64 => value.parse::<i64>().is_ok(),
            Ty::Bool => config::parse_bool(value).is_some(),
        }
    }
    fn describe(self) -> &'static str {
        match self {
            Ty::U64 => "a non-negative integer",
            Ty::I64 => "an integer",
            Ty::Bool => "a boolean (true/false/1/0/on/off)",
        }
    }
}

struct Lever {
    key: &'static str,
    env: Option<&'static str>,
    ty: Ty,
}

/// The live levers an operator may `fleetc settings set`. Paths/binds/forge-target are
/// deliberately absent — structural, bound at startup. Add a toml-mirrored lever here + a
/// typed accessor above; a non-toml runtime lever just reads `resolve_*` directly.
const LEVERS: &[Lever] = &[
    Lever { key: "poll_secs", env: Some("FLEET_POLL_SECS"), ty: Ty::U64 },
    Lever { key: "sync_secs", env: Some("FLEET_SYNC_SECS"), ty: Ty::U64 },
    Lever { key: "reconcile_secs", env: Some("FLEET_RECONCILE_SECS"), ty: Ty::U64 },
    Lever { key: "dry_run", env: Some("FLEET_DRY_RUN"), ty: Ty::Bool },
    Lever { key: "active_secs", env: Some("FLEET_ACTIVE_SECS"), ty: Ty::I64 },
    Lever { key: "quiet_secs", env: Some("FLEET_QUIET_SECS"), ty: Ty::I64 },
];

/// Validate a `settings set key value`: the key must be a known lever and the value must parse
/// as its type. A clear error otherwise — fleetd rejects at the write boundary, never silently.
pub fn validate(key: &str, value: &str) -> Result<(), String> {
    let lever = LEVERS
        .iter()
        .find(|l| l.key == key)
        .ok_or_else(|| format!("unknown live lever {key:?} (known: {})", lever_keys()))?;
    if lever.ty.accepts(value) {
        Ok(())
    } else {
        Err(format!("{key} expects {}, got {value:?}", lever.ty.describe()))
    }
}

fn lever_keys() -> String {
    LEVERS.iter().map(|l| l.key).collect::<Vec<_>>().join(", ")
}

/// One lever's resolved state for `fleetc settings list`: its effective value + where it came from.
#[derive(Debug, Serialize, PartialEq)]
pub struct LeverView {
    pub key: String,
    pub value: String,
    pub source: &'static str, // "env" | "setting" | "config"
}

/// Every lever's effective value + source — the `settings list` read. `source` answers "is my
/// change live?": `env` (a var shadows it), `setting` (a live override is in force), or `config`
/// (fleet.toml/default).
pub fn report(cfg: &Config, l: &Ledger) -> Vec<LeverView> {
    LEVERS
        .iter()
        .map(|lever| {
            let source = if lever.env.map(|e| std::env::var(e).is_ok()).unwrap_or(false) {
                "env"
            } else if setting(l, lever.key).is_some() {
                "setting"
            } else {
                "config"
            };
            LeverView { key: lever.key.into(), value: value_of(cfg, l, lever.key), source }
        })
        .collect()
}

/// Map a lever key to its typed effective value (the one place key→accessor is enumerated).
fn value_of(cfg: &Config, l: &Ledger, key: &str) -> String {
    match key {
        "poll_secs" => poll_secs(cfg, l).to_string(),
        "sync_secs" => sync_secs(cfg, l).to_string(),
        "reconcile_secs" => reconcile_secs(cfg, l).to_string(),
        "dry_run" => dry_run(cfg, l).to_string(),
        "active_secs" => thresholds(cfg, l).active_secs.to_string(),
        "quiet_secs" => thresholds(cfg, l).quiet_secs.to_string(),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A Config with known base values to resolve against.
    fn cfg() -> Config {
        Config::default()
    }

    #[test]
    fn precedence_is_base_then_setting_then_env() {
        let l = Ledger::in_memory().unwrap();
        let c = cfg();
        // nothing set → the base (default) wins.
        assert_eq!(poll_secs(&c, &l), c.poll_secs);
        // a live setting overrides the base.
        l.setting_set("poll_secs", "9").unwrap();
        assert_eq!(poll_secs(&c, &l), 9);
        // env beats the setting (process-pinned override on top).
        std::env::set_var("FLEET_POLL_SECS", "11");
        assert_eq!(poll_secs(&c, &l), 11);
        std::env::remove_var("FLEET_POLL_SECS");
        // and removing env falls back to the setting again.
        assert_eq!(poll_secs(&c, &l), 9);
    }

    #[test]
    fn dry_run_resolves_as_a_bool_lever() {
        let l = Ledger::in_memory().unwrap();
        let c = cfg();
        assert_eq!(dry_run(&c, &l), c.dry_run);
        l.setting_set("dry_run", "true").unwrap();
        assert!(dry_run(&c, &l), "a live setting flips the mode without a restart");
    }

    #[test]
    fn validate_checks_membership_and_type() {
        assert!(validate("poll_secs", "30").is_ok());
        assert!(validate("dry_run", "on").is_ok());
        assert!(validate("active_secs", "-5").is_ok(), "i64 lever accepts a signed value");
        assert!(validate("poll_secs", "soon").is_err(), "u64 lever rejects non-numeric");
        assert!(validate("dry_run", "maybe").is_err(), "bool lever rejects a non-bool");
        assert!(validate("bogus", "1").is_err(), "unknown lever rejected");
    }

    #[test]
    fn report_labels_each_levers_source() {
        let l = Ledger::in_memory().unwrap();
        let c = cfg();
        l.setting_set("sync_secs", "120").unwrap();
        let r = report(&c, &l);
        let sync = r.iter().find(|v| v.key == "sync_secs").unwrap();
        assert_eq!((sync.value.as_str(), sync.source), ("120", "setting"));
        let poll = r.iter().find(|v| v.key == "poll_secs").unwrap();
        assert_eq!(poll.source, "config", "an untouched lever reports its config base");
    }
}
