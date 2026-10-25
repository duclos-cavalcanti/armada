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

use anyhow::{ensure, Context, Result};
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
    /// `[forge]` — whose MRs form the armada. Deployment-specific: no default project, so
    /// `load` fails fast without one.
    pub forge: ForgeConfig,
    /// `[tracker]` — where tickets live. Deployment-specific: no default, `load` fails fast.
    pub tracker: TrackerConfig,
    /// The telemetry DB path (separate from the ledger) + the OTLP/HTTP receiver bind addr.
    pub telemetry_db: String,
    pub otel_addr: String,
    /// `[project]` — the product's main checkout, which owns the ticket worktrees. Captains and
    /// the lookout carry no worktree, so they launch here.
    pub project: ProjectConfig,
}

/// `[project]` in fleet.toml — the product's main checkout. A relative `path` resolves against
/// fleet.toml's directory (not fleetd's cwd), so the kit layout `worktrees/master` just works.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ProjectConfig {
    pub path: String,
}

impl Default for ProjectConfig {
    fn default() -> Self {
        Self { path: "worktrees/master".into() }
    }
}

/// `[forge]` in fleet.toml — which forge adapter, the project on it, and the merge policy.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ForgeConfig {
    pub kind: Option<ForgeKind>,
    /// The forge's project: GitLab's numeric project id, or GitHub's `owner/repo`.
    pub project: String,
    /// The dispatch default: true = the sailor arms auto-merge; false = it lands the MR to clean,
    /// then reports `awaiting_merge` and waits for the operator. `fleetc dispatch --hold` forces false.
    pub auto_merge: bool,
}

impl Default for ForgeConfig {
    fn default() -> Self {
        Self { kind: None, project: String::new(), auto_merge: true }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ForgeKind {
    Gitlab,
    Github,
}

/// `[tracker]` in fleet.toml — which tracker adapter, its project, and the optional claim label.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct TrackerConfig {
    pub kind: Option<TrackerKind>,
    /// The tracker's project. For Jira its key is also the ticket key prefix (`PROJ` → `PROJ-12`).
    pub project: String,
    /// A label a worker's claim adds to the ticket (a team convention); `None` = no label.
    pub claim_label: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TrackerKind {
    Jira,
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
            forge: ForgeConfig::default(),
            tracker: TrackerConfig::default(),
            telemetry_db: wire::default_telemetry_db(),
            otel_addr: "127.0.0.1:4318".into(),
            project: ProjectConfig::default(),
        }
    }
}

impl Config {
    /// Defaults, overlaid by the TOML file (if present), overlaid by env vars — then validated.
    /// A malformed file or a missing `[forge]` is fatal: fleetd never runs on a guessed target.
    pub fn load() -> Result<Self> {
        let mut c = read_file()?.unwrap_or_default();
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
        if let Ok(v) = std::env::var("FLEET_FORGE_PROJECT") {
            c.forge.project = v;
        }
        if let Ok(v) = std::env::var("FLEET_TRACKER_PROJECT") {
            c.tracker.project = v;
        }
        if let Ok(v) = std::env::var("FLEET_TELEMETRY_DB") {
            c.telemetry_db = v;
        }
        if let Ok(v) = std::env::var("FLEET_OTEL_ADDR") {
            c.otel_addr = v;
        }
        c.project.path = resolve_project(&c.project.path);
        c.validate()?;
        Ok(c)
    }

    fn validate(&self) -> Result<()> {
        let path = config_path();
        ensure!(self.forge.kind.is_some(), "{path}: [forge] kind is required (\"gitlab\" or \"github\")");
        ensure!(!self.forge.project.is_empty(), "{path}: [forge] project is required (or FLEET_FORGE_PROJECT)");
        if self.forge.kind == Some(ForgeKind::Github) {
            let parts: Vec<&str> = self.forge.project.split('/').collect();
            ensure!(
                parts.len() == 2 && parts.iter().all(|p| !p.is_empty()),
                "{path}: [forge] project must be \"owner/repo\" for github, got {:?}",
                self.forge.project
            );
        }
        ensure!(self.tracker.kind.is_some(), "{path}: [tracker] kind is required (\"jira\")");
        ensure!(
            domain::is_key_prefix(&self.tracker.project),
            "{path}: [tracker] project must be a key prefix like \"PROJ\" (or FLEET_TRACKER_PROJECT), got {:?}",
            self.tracker.project
        );
        Ok(())
    }

    /// The ticket key prefix — how branches, MR titles, and keys name a ticket. For Jira it is
    /// the project key; a tracker with its own key scheme would supply its own here.
    pub fn ticket_prefix(&self) -> &str {
        &self.tracker.project
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

/// A relative `[project] path` is relative to fleet.toml's directory. Not canonicalized — the
/// checkout may not exist yet (preflight reports that, with the clone command).
fn resolve_project(path: &str) -> String {
    let p = std::path::Path::new(path);
    if p.is_absolute() {
        return path.to_string();
    }
    let joined = std::path::Path::new(&config_path()).parent().unwrap_or(std::path::Path::new("")).join(p);
    std::path::absolute(&joined).unwrap_or(joined).to_string_lossy().into_owned()
}

/// The parsed file, or `None` when there is none. A file that exists but won't read or
/// parse is an error — never silently ignored.
fn read_file() -> Result<Option<Config>> {
    let path = config_path();
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("reading {path}")),
    };
    toml::from_str(&text).map(Some).with_context(|| format!("parsing {path}"))
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

    const FORGE: &str = "[forge]\nkind = \"gitlab\"\nproject = \"42\"\n";
    const TRACKER: &str = "[tracker]\nkind = \"jira\"\nproject = \"PROJ\"\n";

    #[test]
    fn forge_table_parses_and_validates() {
        let c: Config = toml::from_str(&format!("{FORGE}{TRACKER}")).unwrap();
        assert_eq!(c.forge.kind, Some(ForgeKind::Gitlab));
        assert_eq!(c.forge.project, "42");
        assert!(c.validate().is_ok());
    }

    #[test]
    fn forge_auto_merge_defaults_on_and_can_be_turned_off() {
        let c: Config = toml::from_str(&format!("{FORGE}{TRACKER}")).unwrap();
        assert!(c.forge.auto_merge, "the sailor arms auto-merge unless the deployment says otherwise");
        let c: Config = toml::from_str(&format!("{FORGE}auto_merge = false\n{TRACKER}")).unwrap();
        assert!(!c.forge.auto_merge);
    }

    #[test]
    fn tracker_table_parses_with_an_optional_claim_label() {
        let c: Config = toml::from_str(&format!("{FORGE}{TRACKER}")).unwrap();
        assert_eq!((c.tracker.kind, c.ticket_prefix()), (Some(TrackerKind::Jira), "PROJ"));
        assert_eq!(c.tracker.claim_label, None, "no label unless configured");
        let c: Config = toml::from_str(&format!("{FORGE}{TRACKER}claim_label = \"Team-X\"\n")).unwrap();
        assert_eq!(c.tracker.claim_label.as_deref(), Some("Team-X"));
    }

    #[test]
    fn tracker_is_required_and_its_project_must_be_a_key_prefix() {
        let c: Config = toml::from_str(FORGE).unwrap();
        assert!(c.validate().is_err(), "no [tracker]");
        let c: Config = toml::from_str(&format!("{FORGE}[tracker]\nkind = \"jira\"\nproject = \"proj x\"\n")).unwrap();
        assert!(c.validate().is_err(), "a project that can't prefix a key");
    }

    #[test]
    fn forge_is_required() {
        // No built-in default target: the empty config, or a kind without a project, fails.
        assert!(Config::default().validate().is_err());
        let c: Config = toml::from_str(&format!("[forge]\nkind = \"gitlab\"\n{TRACKER}")).unwrap();
        assert!(c.validate().is_err(), "a forge without a project");
    }

    #[test]
    fn github_forge_needs_an_owner_repo_project() {
        let gh = |project: &str| format!("[forge]\nkind = \"github\"\nproject = \"{project}\"\n{TRACKER}");
        let c: Config = toml::from_str(&gh("me/armada-test")).unwrap();
        assert_eq!(c.forge.kind, Some(ForgeKind::Github));
        assert!(c.validate().is_ok());
        let c: Config = toml::from_str(&gh("12345678")).unwrap();
        assert!(c.validate().is_err(), "a GitLab-style id isn't a GitHub repo");
    }

    #[test]
    fn unknown_forge_kind_is_a_parse_error() {
        assert!(toml::from_str::<Config>("[forge]\nkind = \"bitbucket\"\nproject = \"1\"\n").is_err());
    }

    #[test]
    fn project_path_resolves_against_the_config_dir() {
        assert_eq!(resolve_project("/abs/checkout"), "/abs/checkout");
        let rel = resolve_project("worktrees/master");
        assert!(std::path::Path::new(&rel).is_absolute() && rel.ends_with("worktrees/master"), "{rel}");
    }

    #[test]
    fn dry_run_can_be_turned_on_in_the_file() {
        let c: Config = toml::from_str("dry_run = true\n").unwrap();
        assert!(c.dry_run, "the parity look is opt-in via config/env");
    }
}
