//! The `tmux` SessionHost driver — shells out to `tmux` (PLAN: the CLI holds the
//! auth/lore, so we reuse it rather than reimplement it). Swap this impl for a
//! `DockerSessionHost`/`SshSessionHost` later without touching `core`/`fleetd`.

use anyhow::{ensure, Result};
use domain::ports::SessionHost;
use std::path::Path;
use std::process::{Command, Output};

/// Spawns cogs as detached tmux sessions (shelling to the external `tmux` CLI), injecting the
/// per-cog OpenTelemetry *attribution* at spawn. The static OTEL env (enable + endpoint + interval)
/// lives in `~/.claude/settings.json` `env`, so it covers EVERY claude session (fleet + one-off);
/// only the attribution — which a static settings map can't vary per session — is injected here.
/// Liveness/kill/send also shell to tmux.
pub struct TmuxHost;

impl TmuxHost {
    pub fn new() -> Self {
        TmuxHost
    }
}

impl Default for TmuxHost {
    fn default() -> Self {
        Self::new()
    }
}

/// The per-cog OpenTelemetry attribution injected at spawn: `OTEL_SERVICE_NAME` + the
/// `mady.role`/`mady.ticket` resource attributes, parsed from the session name (`worker-MADY-1`
/// → role=worker, ticket=MADY-1; a standing cog like `foreman` → role only). Always injected —
/// inert when telemetry is off. A one-off `claude` gets none → NULL role at the collector (the
/// "interactive" bucket). This is the one piece settings.json can't provide (it's static).
fn otel_attribution(session_name: &str) -> Vec<(&'static str, String)> {
    let (role, ticket) = match session_name.split_once("-MADY-") {
        Some((role, num)) => (role.to_string(), Some(format!("MADY-{num}"))),
        None => (session_name.to_string(), None),
    };
    let mut attrs = format!("mady.role={role}");
    if let Some(t) = &ticket {
        attrs.push_str(&format!(",mady.ticket={t}"));
    }
    vec![("OTEL_SERVICE_NAME", format!("mady-{role}")), ("OTEL_RESOURCE_ATTRIBUTES", attrs)]
}

fn tmux(args: &[&str]) -> Result<Output> {
    Ok(Command::new("tmux").args(args).output()?)
}

/// `=name` is tmux's exact-match target, so a session name is never a prefix match.
fn target(name: &str) -> String {
    format!("={name}")
}

impl SessionHost for TmuxHost {
    fn spawn(&self, name: &str, cwd: &Path, cmd: &str) -> Result<()> {
        // A detached tmux session running `cmd` in `cwd` — the Rust re-iteration of
        // launcher.sh's spawn. config.env sourcing is intentionally NOT replicated: it's
        // redundant (kit scripts re-source it via bootstrap.sh; no agent reads MADY_* from the
        // env). OTEL, which launcher.sh also handled, IS replicated below when enabled.
        let mut tmux = Command::new("tmux");
        tmux.args(["new-session", "-d", "-s", name, "-n", "main"]);
        // Per-cog OpenTelemetry attribution (tmux 3.0+ `-e`) — tags the cog's emissions by role
        // + ticket. Inert when telemetry is off; the enable/endpoint come from settings.json.
        for (k, v) in otel_attribution(name) {
            tmux.arg("-e").arg(format!("{k}={v}"));
        }
        tmux.arg("-c").arg(cwd).arg(cmd);
        let status = tmux.status()?;
        ensure!(status.success(), "tmux new-session failed for {name}");
        Ok(())
    }

    fn alive(&self, name: &str) -> Result<bool> {
        Ok(tmux(&["has-session", "-t", &target(name)])?.status.success())
    }

    fn last_activity(&self, name: &str) -> Result<Option<i64>> {
        let out = tmux(&["display-message", "-p", "-t", &target(name), "#{window_activity}"])?;
        if !out.status.success() {
            return Ok(None);
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().parse::<i64>().ok())
    }

    fn kill(&self, name: &str) -> Result<()> {
        tmux(&["kill-session", "-t", &target(name)])?;
        Ok(())
    }

    fn send(&self, name: &str, text: &str) -> Result<()> {
        tmux(&["send-keys", "-t", &target(name), text, "Enter"])?;
        Ok(())
    }

    fn list(&self) -> Result<Vec<String>> {
        let out = tmux(&["list-sessions", "-F", "#{session_name}"])?;
        // No tmux server / no sessions is not an error — it's an empty fleet.
        if !out.status.success() {
            return Ok(vec![]);
        }
        Ok(String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(|l| l.to_string())
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn attribution_tags_role_and_ticket_from_the_session_name() {
        let env: HashMap<_, _> = otel_attribution("worker-MADY-1").into_iter().collect();
        assert_eq!(env["OTEL_SERVICE_NAME"], "mady-worker");
        assert_eq!(env["OTEL_RESOURCE_ATTRIBUTES"], "mady.role=worker,mady.ticket=MADY-1");
    }

    #[test]
    fn attribution_for_a_standing_cog_has_role_but_no_ticket() {
        let env: HashMap<_, _> = otel_attribution("foreman").into_iter().collect();
        assert_eq!(env["OTEL_SERVICE_NAME"], "mady-foreman");
        assert_eq!(env["OTEL_RESOURCE_ATTRIBUTES"], "mady.role=foreman");
    }
}
