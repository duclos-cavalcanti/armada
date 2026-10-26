//! Preflight — before fleetd goes ONLINE (and on `fleetd preflight`), every adapter verifies its own
//! surface: its CLI is installed, authenticated, and reaches the configured target; the project
//! checkout exists. One ✓/✗ line per check; any ✗ stops the boot with the whole list, so a broken
//! setup fails at the door, not mid-sync.

use anyhow::{anyhow, bail, Result};
use std::process::Command;

/// Run `program args…` and return its trimmed stdout. A missing binary is its own clear error
/// (`brew install <program>`), distinct from the command failing.
pub(crate) fn cli(program: &str, args: &[&str]) -> Result<String> {
    let out = Command::new(program).args(args).output().map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => anyhow!("{program} not found on PATH (brew install {program})"),
        _ => anyhow!("{program}: {e}"),
    })?;
    if !out.status.success() {
        let err = strip_ansi(&String::from_utf8_lossy(&out.stderr));
        bail!("{program} {} failed: {}", args.join(" "), headline(&err));
    }
    Ok(strip_ansi(&String::from_utf8_lossy(&out.stdout)).trim().to_string())
}

/// The line of a CLI's stderr that names the problem: the first error-looking one (`x …`, `error`,
/// `failed`, `token`…), else the first non-empty one. CLIs lead with headers (`gitlab.com`).
fn headline(err: &str) -> String {
    const CUES: [&str; 6] = ["error", "fail", "token", "unauthorized", "denied", "not found"];
    let lines = || err.lines().map(str::trim).filter(|l| !l.is_empty());
    lines()
        .find(|l| l.starts_with("x ") || CUES.iter().any(|c| l.to_ascii_lowercase().contains(c)))
        .or_else(|| lines().next())
        .unwrap_or("non-zero exit")
        .to_string()
}

/// A CLI's stderr as one log-friendly line: ANSI colour stripped, wrapped lines and blank runs
/// collapsed to single spaces.
pub(crate) fn one_line(stderr: &[u8]) -> String {
    strip_ansi(&String::from_utf8_lossy(stderr)).split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The last `lines` lines of a CLI log, ANSI colour stripped — a failed CI job's tail.
pub(crate) fn log_tail(log: &str, lines: usize) -> String {
    let clean = strip_ansi(log);
    let all: Vec<&str> = clean.lines().collect();
    all[all.len().saturating_sub(lines)..].join("\n")
}

/// Drop ANSI colour escapes (`ESC [ … letter`) so CLI output reads cleanly in the checklist.
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            chars.by_ref().find(|c| c.is_ascii_alphabetic()); // skip to the final letter
        } else {
            out.push(c);
        }
    }
    out
}

/// The project's runtime state: `.fleet/` exists (created on first run) and is writable, the socket
/// path fits a Unix socket (macOS caps it at 104 bytes), and an existing ledger opens.
pub fn runtime(project: &wire::Project) -> Result<String> {
    let dir = project.runtime();
    std::fs::create_dir_all(&dir).map_err(|e| anyhow!("can't create {}: {e}", dir.display()))?;
    let probe = dir.join(".write-probe");
    std::fs::write(&probe, b"").map_err(|e| anyhow!("{} isn't writable: {e}", dir.display()))?;
    let _ = std::fs::remove_file(&probe);
    let socket = project.socket();
    if socket.len() >= 104 {
        bail!("socket path is {} bytes, over the 103 a Unix socket allows: {socket} — move the repo shallower", socket.len());
    }
    let ledger = project.ledger();
    let state = if std::path::Path::new(&ledger).exists() {
        ledger::Ledger::open_readonly(&ledger).map_err(|e| anyhow!("ledger {ledger} won't open: {e:#}"))?;
        "ledger ok"
    } else {
        "new ledger"
    };
    Ok(format!("{} · {state}", dir.display()))
}

/// Print the checklist; `Err` (after printing it) if any check failed.
pub fn report(checks: &[(&str, Result<String>)]) -> Result<()> {
    print!("{}", render(checks));
    verdict(checks)
}

/// Log the checklist, one event per check; `Err` if any check failed.
pub fn log(checks: &[(&str, Result<String>)]) -> Result<()> {
    for (name, r) in checks {
        match r {
            Ok(ok) => tracing::info!("✓ {name:<8} {ok}"),
            Err(e) => tracing::error!("✗ {name:<8} {e:#}"),
        }
    }
    verdict(checks)
}

fn verdict(checks: &[(&str, Result<String>)]) -> Result<()> {
    let failed = checks.iter().filter(|(_, r)| r.is_err()).count();
    if failed > 0 {
        bail!("preflight: {failed} of {} checks failed", checks.len());
    }
    Ok(())
}

fn render(checks: &[(&str, Result<String>)]) -> String {
    checks
        .iter()
        .map(|(name, r)| match r {
            Ok(ok) => format!("  ✓ {name:<8} {ok}\n"),
            Err(e) => format!("  ✗ {name:<8} {e:#}\n"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_lists_every_check_and_fails_on_any_cross() {
        let checks = [("forge", Ok("glab · project a/b".to_string())), ("tracker", Err(anyhow!("jira not found on PATH")))];
        let out = render(&checks);
        assert!(out.contains("✓ forge") && out.contains("glab · project a/b"));
        assert!(out.contains("✗ tracker") && out.contains("jira not found on PATH"));
        assert!(report(&checks).is_err());
        assert!(report(&checks[..1]).is_ok());
    }

    #[test]
    fn one_line_collapses_a_wrapped_cli_error() {
        let glab = b"   ERROR\n\n  Get \"https://x/2327\": read: connection reset by\n  peer.\n\n";
        assert_eq!(one_line(glab), "ERROR Get \"https://x/2327\": read: connection reset by peer.");
    }

    #[test]
    fn headline_picks_the_error_line_and_strips_colour() {
        let glab = "gitlab.com\n  x gitlab.com: API call failed: 401\n  ✓ Git operations configured";
        assert_eq!(headline(glab), "x gitlab.com: API call failed: 401");
        assert_eq!(strip_ansi("\x1b[0;33mThe tool needs a Jira API token\x1b[0m"), "The tool needs a Jira API token");
        assert_eq!(headline("just one line"), "just one line");
    }

    #[test]
    fn cli_reports_a_missing_binary_plainly() {
        let e = cli("definitely-not-a-real-binary-xyz", &[]).unwrap_err().to_string();
        assert!(e.contains("not found on PATH"), "{e}");
    }
}
