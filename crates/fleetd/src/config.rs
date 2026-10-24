//! fleetd config — the STARTUP base. Precedence: **built-in defaults < config file
//! (TOML) < env vars**, folded into one `Config` at launch. Defaults mean it runs with
//! zero config; the file is for persistent tuning without a recompile; env vars are
//! per-run overrides.
//!
//! The LIVE overlay (the `settings` module) sits on top: a lever an operator patches at
//! runtime resolves `base(this) < setting < env` and takes effect with no restart — so the
//! full precedence is **defaults < toml < settings < env**. This file owns the base; a
//! live lever reads its `self.<field>` here as the fall-through.
//!
//! Consolidates what used to be scattered `env::var` calls + hardcoded consts into
//! one place, injected at startup. Extend by adding a field + a default + (optionally)
//! an env override — the same pattern serves every future lever.

use domain::LivenessThresholds;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(default)] // any field absent from the TOML falls back to Default::default()
pub struct Config {
    pub ledger: String,
    pub socket: String,
    pub poll_secs: u64,
    pub sync_secs: u64,
    pub reconcile_secs: u64,
    /// Reconcile mode: false (default, post-cutover) = apply the plan through the ports — fleetd
    /// drives the line; true = compute + log only, execute nothing (the parity look, opt in via
    /// `FLEET_DRY_RUN=true`). Flip via config/env, no recompile.
    pub dry_run: bool,
    pub active_secs: i64,
    pub quiet_secs: i64,
    pub gitlab_project: String,
    /// The telemetry DB path (separate from the ledger) + the OTLP/HTTP receiver bind addr.
    pub telemetry_db: String,
    pub otel_addr: String,
    /// The hub — the main-repo working dir where standing crew (captains, and later
    /// auditor/shipwright) launch (the bash kit's `$MADY_REPOSITORY`). Captains carry no
    /// per-ticket worktree, so they run here.
    pub hub: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            ledger: wire::default_ledger(), // shared with fleetc plan (DRY the path)
            socket: wire::default_socket(), // shared with every client (DRY the path)
            poll_secs: 5,
            sync_secs: 300,
            reconcile_secs: 60,
            dry_run: false,
            active_secs: domain::LIVENESS_ACTIVE_SECS,
            quiet_secs: domain::LIVENESS_QUIET_SECS,
            gitlab_project: "12345678".into(),
            telemetry_db: wire::default_telemetry_db(),
            otel_addr: "127.0.0.1:4318".into(),
            hub: std::env::var("MADY_REPOSITORY").unwrap_or_else(|_| ".".into()),
        }
    }
}

impl Config {
    /// Defaults, overlaid by the TOML file (if present), overlaid by env vars.
    pub fn load() -> Self {
        let mut c = read_file().unwrap_or_default();
        if let Ok(v) = std::env::var("FLEET_LEDGER") {
            c.ledger = v;
        }
        if let Ok(v) = std::env::var("FLEET_SOCK") {
            c.socket = v;
        }
        if let Some(v) = env_num("FLEET_POLL_SECS") {
            c.poll_secs = v;
        }
        if let Some(v) = env_num("FLEET_SYNC_SECS") {
            c.sync_secs = v;
        }
        if let Some(v) = env_num("FLEET_RECONCILE_SECS") {
            c.reconcile_secs = v;
        }
        if let Some(v) = env_bool("FLEET_DRY_RUN") {
            c.dry_run = v;
        }
        if let Some(v) = env_num("FLEET_ACTIVE_SECS") {
            c.active_secs = v as i64;
        }
        if let Some(v) = env_num("FLEET_QUIET_SECS") {
            c.quiet_secs = v as i64;
        }
        if let Ok(v) = std::env::var("FLEET_GITLAB_PROJECT") {
            c.gitlab_project = v;
        }
        if let Ok(v) = std::env::var("FLEET_TELEMETRY_DB") {
            c.telemetry_db = v;
        }
        if let Ok(v) = std::env::var("FLEET_OTEL_ADDR") {
            c.otel_addr = v;
        }
        if let Ok(v) = std::env::var("FLEET_HUB") {
            c.hub = v;
        }
        c
    }

    pub fn thresholds(&self) -> LivenessThresholds {
        LivenessThresholds { active_secs: self.active_secs, quiet_secs: self.quiet_secs }
    }
}

fn config_path() -> String {
    // fleet.toml lives in the repo — fleetd is its only reader and runs from the repo.
    // `make run` sets $FLEET_CONFIG=<repo>/fleet.toml; the cwd-relative fallback covers
    // running fleetd from the repo directly. (~/.fleet holds only runtime state, no config.)
    std::env::var("FLEET_CONFIG").unwrap_or_else(|_| "fleet.toml".into())
}

fn read_file() -> Option<Config> {
    let path = config_path();
    let text = std::fs::read_to_string(&path).ok()?;
    match toml::from_str(&text) {
        Ok(c) => Some(c),
        Err(e) => {
            eprintln!("fleetd: ignoring bad config {path}: {e}");
            None
        }
    }
}

/// Read a numeric env var. `pub(crate)` so the live `settings` overlay resolves env the same way.
pub(crate) fn env_num(var: &str) -> Option<u64> {
    std::env::var(var).ok().and_then(|s| s.parse().ok())
}

/// Read a boolean env var (via `parse_bool`). `pub(crate)` for the `settings` overlay.
pub(crate) fn env_bool(var: &str) -> Option<bool> {
    parse_bool(&std::env::var(var).ok()?)
}

/// Parse a boolean: 1/0, true/false, yes/no, on/off (case-insensitive). Shared by env + settings
/// parsing so both accept the same spellings.
pub(crate) fn parse_bool(s: &str) -> Option<bool> {
    match s.to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_sane() {
        let c = Config::default();
        assert_eq!(c.poll_secs, 5);
        assert_eq!(c.sync_secs, 300);
        assert_eq!(c.reconcile_secs, 60);
        assert!(!c.dry_run, "live is the default post-cutover (dry-run is opt-in)");
        assert_eq!(c.active_secs, 120);
        assert_eq!(c.quiet_secs, 1800);
        assert!(c.ledger.ends_with("ledger.db"));
        assert!(c.socket.ends_with("fleetd.sock"));
        assert!(c.telemetry_db.ends_with("telemetry.db"));
        assert_eq!(c.otel_addr, "127.0.0.1:4318");
    }

    #[test]
    fn toml_overrides_only_the_present_fields() {
        // A partial config file: present fields win, absent fields keep defaults.
        let c: Config = toml::from_str("poll_secs = 9\nactive_secs = 42\n").unwrap();
        assert_eq!(c.poll_secs, 9); // from file
        assert_eq!(c.active_secs, 42); // from file
        assert_eq!(c.sync_secs, 300); // default preserved
        assert_eq!(c.quiet_secs, 1800); // default preserved
        assert!(!c.dry_run); // default preserved (absent from the file)
    }

    #[test]
    fn dry_run_can_be_turned_on_in_the_file() {
        let c: Config = toml::from_str("dry_run = true\n").unwrap();
        assert!(c.dry_run, "the parity look is opt-in via config/env");
    }
}
