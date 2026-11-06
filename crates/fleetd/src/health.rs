//! The fleet's health check: the guards on the project's shared git state, and that state itself.
//! fleetd runs it at boot, where a miss refuses to come online, and after every sync, where a miss
//! holds the lookouts until it passes (the dashboard's `fleet:` light goes out). It checks that:
//! - the kit's ref guard (`hooks/git-hook-reference-transaction`) is linked into the checkout's git
//!   hooks (`./install.sh --hooks`), and no `core.hooksPath` sends git elsewhere;
//! - the lookouts' command guard (`hooks/claude-hook-lookout-guard`) is there to attach;
//! - the checkout's own git settings carry no `core.worktree` and no `user.name` / `user.email`:
//!   what a test that borrowed the checkout's `.git` leaves behind, and git runs no hook for.

use anyhow::{bail, Result};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The kit's guards, linked or attached from here.
pub const REF_GUARD: &str = "git-hook-reference-transaction";
pub const LOOKOUT_GUARD: &str = "claude-hook-lookout-guard";

/// The kit's `hooks/` folder. fleetd is built from the kit, so it's found from the build tree.
pub fn kit_hooks() -> PathBuf {
    let hooks = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../hooks");
    hooks.canonicalize().unwrap_or(hooks)
}

/// Every way the project's git state, or its guards, are off; empty when healthy.
pub fn problems(checkout: &str) -> Vec<String> {
    let mut out = Vec::new();
    let git_dir = Path::new(checkout).join(".git");
    if !git_dir.is_dir() {
        return vec![format!("no git checkout at {checkout}")];
    }
    let want = kit_hooks().join(REF_GUARD).canonicalize().ok();
    let link = git_dir.join("hooks/reference-transaction");
    if want.is_none() || link.canonicalize().ok() != want {
        out.push("the ref guard isn't linked into the checkout: run ./install.sh --hooks".to_string());
    } else if !executable(&link) {
        out.push("the ref guard isn't executable".to_string());
    }
    let config = git_dir.join("config");
    for (scope, key) in [(Some(&config), "core.hooksPath"), (None, "core.hooksPath")] {
        if let Some(v) = setting(scope.map(|p| p.as_path()), key) {
            out.push(format!("{key} is set ({v}), so git skips the ref guard"));
        }
    }
    for key in ["core.worktree", "user.name", "user.email"] {
        if let Some(v) = setting(Some(&config), key) {
            out.push(format!("the checkout's git settings carry {key} = {v} (a borrowed .git?): git config --file {} --unset {key}", config.display()));
        }
    }
    if !executable(&kit_hooks().join(LOOKOUT_GUARD)) {
        out.push(format!("the lookouts' command guard is missing: {}", kit_hooks().join(LOOKOUT_GUARD).display()));
    }
    out
}

/// The boot check, for preflight's checklist: a miss refuses to come online.
pub fn preflight(checkout: &str) -> Result<String> {
    let problems = problems(checkout);
    if !problems.is_empty() {
        bail!("{}", problems.join("; "));
    }
    Ok("ref guard linked · lookout guard present · git settings clean".into())
}

/// The `--settings` a lookout launches with: the command guard on its every Bash call.
pub fn lookout_settings() -> String {
    let guard = kit_hooks().join(LOOKOUT_GUARD);
    serde_json::json!({ "hooks": { "PreToolUse": [{ "matcher": "Bash", "hooks": [{ "type": "command", "command": guard }] }] } })
        .to_string()
}

/// One git setting from `file` (the global one when `None`), read with no repository in play, so a
/// broken `core.worktree` can't stop the read that finds it.
fn setting(file: Option<&Path>, key: &str) -> Option<String> {
    let mut cmd = Command::new("git");
    cmd.current_dir("/").arg("config");
    match file {
        Some(f) => cmd.arg("--file").arg(f),
        None => cmd.arg("--global"),
    };
    let out = cmd.args(["--get", key]).output().ok()?;
    let v = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !v.is_empty()).then_some(v)
}

fn executable(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_checkout_is_healthy_once_guarded_and_clean_and_each_miss_is_named() {
        let (dir, checkout, _) = crate::git::fixture::checkout("health", &[]);
        let git = |args: &[&str]| assert!(Command::new("git").arg("-C").arg(&checkout).args(args).status().unwrap().success());
        let p = problems(&checkout);
        assert!(p.len() == 1 && p[0].contains("./install.sh --hooks"), "unguarded: {p:?}");

        std::os::unix::fs::symlink(kit_hooks().join(REF_GUARD), Path::new(&checkout).join(".git/hooks/reference-transaction")).unwrap();
        assert_eq!(problems(&checkout), Vec::<String>::new(), "guarded and clean");
        assert!(preflight(&checkout).is_ok());

        git(&["config", "user.name", "test"]);
        git(&["config", "core.worktree", "/nowhere/at/all"]);
        let p = problems(&checkout);
        assert!(p.iter().any(|m| m.contains("user.name = test")), "{p:?}");
        assert!(p.iter().any(|m| m.contains("core.worktree = /nowhere/at/all")), "found even though it breaks git: {p:?}");
        assert!(preflight(&checkout).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_lookout_launches_with_the_command_guard_on_bash() {
        let s: serde_json::Value = serde_json::from_str(&lookout_settings()).unwrap();
        assert_eq!(s.pointer("/hooks/PreToolUse/0/matcher").and_then(|m| m.as_str()), Some("Bash"));
        let cmd = s.pointer("/hooks/PreToolUse/0/hooks/0/command").and_then(|c| c.as_str()).unwrap();
        assert!(cmd.ends_with("hooks/claude-hook-lookout-guard") && executable(Path::new(cmd)), "{cmd}");
    }
}
