//! The fleet's health check: one named check each on what the fleet runs on, grouped. fleetd runs it
//! at boot, where a `Fail` refuses to come online, and after every sync, where a `Fail` holds the
//! lookouts until it passes (the dashboard's `fleet:` light goes out) and a `Warn` only lights the
//! dashboard. Each change is recorded (`health_checked`), and the dashboard's Health tab shows it.
//! - **Kit**: its agents are linked into `~/.claude/agents`; the lookouts' command guard is there.
//! - **Git**: the checkout is a git repository; the kit's ref guard is linked into its hooks and no
//!   `core.hooksPath` skips it; its git settings carry no `core.worktree`, `user.name` or
//!   `user.email` (what a test that borrowed its `.git` leaves behind); and it follows origin, on its
//!   default branch, fast-forwarded. The check advances it: captains and admirals run there.
//! - **Project Skills**: the skills fleet.toml names exist.
//! - **Project Worktrees**: no stale worktree entry blocks cutting one again.

use crate::config::Config;
use anyhow::{bail, Result};
use domain::{HealthCheck, HealthStatus};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The kit's guards, linked or attached from here.
pub const REF_GUARD: &str = "git-hook-reference-transaction";
pub const LOOKOUT_GUARD: &str = "claude-hook-lookout-guard";
/// The kit agents a review needs: a missing one holds the lookouts; any other only warns.
const REVIEW_AGENTS: [&str; 2] = ["lookout.md", "spyglass.md"];

/// The kit's `hooks/` folder. fleetd is built from the kit, so it's found from the build tree.
pub fn kit_hooks() -> PathBuf {
    let hooks = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../hooks");
    hooks.canonicalize().unwrap_or(hooks)
}

/// Every check, in the Health tab's order.
pub fn checks(cfg: &Config) -> Vec<HealthCheck> {
    let checkout = cfg.project.path.as_str();
    let mut out = vec![kit_agents(), lookout_guard()];
    out.extend(git(checkout));
    out.extend([
        skill(checkout, "review-skill", "the review skill fleet.toml names exists", cfg.review_skill.as_deref(), HealthStatus::Fail, "the kit's /fleet-review runs"),
        skill(checkout, "grooming-skill", "the grooming skill fleet.toml names exists", cfg.groom_skill.as_deref(), HealthStatus::Warn, "an admiral grooms by /fleet-ready-done"),
        skill(checkout, "dor-skill", "the readiness check fleet.toml names exists", cfg.assess_skill.as_deref(), HealthStatus::Warn, "Ready is judged by /fleet-ready-done"),
        worktrees(checkout),
    ]);
    out
}

/// The failing checks' details: what holds the lookouts. Empty when healthy.
pub fn problems(checks: &[HealthCheck]) -> Vec<String> {
    checks
        .iter()
        .filter(|c| c.status == HealthStatus::Fail)
        .map(|c| format!("{}: {}", c.name, c.detail.as_deref().unwrap_or("failed")))
        .collect()
}

/// The boot check, for preflight's checklist: a `Fail` refuses to come online.
pub fn preflight(cfg: &Config) -> Result<String> {
    let checks = checks(cfg);
    let problems = problems(&checks);
    if !problems.is_empty() {
        bail!("{}", problems.join("; "));
    }
    let count = |s| checks.iter().filter(|c| c.status == s).count();
    Ok(format!("{} ok · {} warn · {} unset", count(HealthStatus::Ok), count(HealthStatus::Warn), count(HealthStatus::Unset)))
}

fn check(group: &str, name: &str, about: &str, status: HealthStatus, detail: Option<String>) -> HealthCheck {
    HealthCheck { group: group.into(), name: name.into(), about: about.into(), status, detail }
}

/// Ok when there's nothing to say, else `status` with why.
fn verdict(group: &str, name: &str, about: &str, status: HealthStatus, detail: Option<String>) -> HealthCheck {
    match detail {
        None => check(group, name, about, HealthStatus::Ok, None),
        Some(d) => check(group, name, about, status, Some(d)),
    }
}

// ── Kit ─────────────────────────────────────────────────────────────────────────────

fn kit_agents() -> HealthCheck {
    let about = "each kit agent is linked into ~/.claude/agents";
    let agents = kit_hooks().join("../agents");
    let linked = std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".claude/agents"));
    let mut missing: Vec<String> = std::fs::read_dir(&agents)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "md"))
        .filter(|p| {
            let name = p.file_name().unwrap_or_default();
            linked.as_ref().and_then(|l| l.join(name).canonicalize().ok()) != p.canonicalize().ok()
        })
        .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .collect();
    missing.sort();
    if missing.is_empty() {
        return check("Kit", "agents", about, HealthStatus::Ok, None);
    }
    let status = if missing.iter().any(|m| REVIEW_AGENTS.contains(&m.as_str())) { HealthStatus::Fail } else { HealthStatus::Warn };
    check("Kit", "agents", about, status, Some(format!("not linked: {}: run ./install.sh", missing.join(", "))))
}

fn lookout_guard() -> HealthCheck {
    let guard = kit_hooks().join(LOOKOUT_GUARD);
    let miss = (!executable(&guard)).then(|| format!("missing or not executable: {}", guard.display()));
    verdict("Kit", "lookout-guard", "the lookouts' command guard is installed", HealthStatus::Fail, miss)
}

// ── Git ─────────────────────────────────────────────────────────────────────────────

fn git(checkout: &str) -> Vec<HealthCheck> {
    let git_dir = Path::new(checkout).join(".git");
    if !git_dir.is_dir() {
        return vec![check("Git", "checkout", "the project checkout is a git repository", HealthStatus::Fail, Some(format!("no git checkout at {checkout}")))];
    }
    let mut out = vec![check("Git", "checkout", "the project checkout is a git repository", HealthStatus::Ok, None)];

    let want = kit_hooks().join(REF_GUARD).canonicalize().ok();
    let link = git_dir.join("hooks/reference-transaction");
    let miss = if want.is_none() || link.canonicalize().ok() != want {
        Some("not linked into the checkout: run ./install.sh --hooks".to_string())
    } else if !executable(&link) {
        Some("not executable".to_string())
    } else {
        None
    };
    out.push(verdict("Git", "ref-guard", "the ref guard is linked into the checkout's hooks", HealthStatus::Fail, miss));

    let config = git_dir.join("config");
    let set: Vec<String> = [(Some(config.as_path()), "core.hooksPath"), (None, "core.hooksPath")]
        .into_iter()
        .filter_map(|(scope, key)| setting(scope, key).map(|v| format!("{key} = {v} ({})", if scope.is_some() { "the checkout's" } else { "global" })))
        .collect();
    let miss = (!set.is_empty()).then(|| format!("{}, so git skips the ref guard: unset it", set.join("; ")));
    out.push(verdict("Git", "hooks-path", "no core.hooksPath lets git skip the ref guard", HealthStatus::Fail, miss));

    let carried: Vec<String> = ["core.worktree", "user.name", "user.email"]
        .into_iter()
        .filter_map(|key| setting(Some(&config), key).map(|v| format!("{key} = {v}")))
        .collect();
    let miss = (!carried.is_empty())
        .then(|| format!("carries {} (a borrowed .git?): git config --file {} --unset <key>", carried.join(", "), config.display()));
    out.push(verdict("Git", "git-settings", "the checkout's git config is clean", HealthStatus::Fail, miss));

    let about = "the checkout is on origin's default branch, fast-forwarded";
    if out.iter().all(|c| c.status == HealthStatus::Ok) {
        out.push(match follow_origin(checkout) {
            Some((status, why)) => check("Git", "follows-origin", about, status, Some(why)),
            None => check("Git", "follows-origin", about, HealthStatus::Ok, None),
        });
    } else {
        // Only a guarded checkout with clean settings is moved.
        out.push(check("Git", "follows-origin", about, HealthStatus::Unset, Some("not run until the git checks above pass".into())));
    }
    out
}

/// Fast-forward the checkout to origin's default branch, or say what keeps it from following: another
/// branch, an uncommitted change, or a divergence (a failure), or a failed fetch (a warning: the
/// network, most likely; the next check retries). Never forces anything.
fn follow_origin(checkout: &str) -> Option<(HealthStatus, String)> {
    let fail = |why: String| Some((HealthStatus::Fail, why));
    let default = match run_git(checkout, &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"]) {
        Ok(d) => d,
        Err(_) => return fail("origin/HEAD doesn't resolve in the checkout: git remote set-head origin --auto".into()),
    };
    let branch = default.strip_prefix("origin/").unwrap_or(&default).to_string();
    match run_git(checkout, &["symbolic-ref", "--short", "HEAD"]) {
        Ok(b) if b == branch => {}
        Ok(b) => return fail(format!("on {b}, not {branch}, so it can't follow {default}: git switch {branch}")),
        Err(_) => return fail(format!("its HEAD is detached, so it can't follow {default}: git switch {branch}")),
    }
    if run_git(checkout, &["status", "--porcelain", "--untracked-files=no"]).is_ok_and(|s| !s.is_empty()) {
        return fail(format!("uncommitted changes, so it can't follow {default}: commit or discard them"));
    }
    let refspec = format!("+refs/heads/{branch}:refs/remotes/origin/{branch}");
    if let Err(e) = run_git(checkout, &["fetch", "--quiet", "--no-tags", "origin", &refspec]) {
        return Some((HealthStatus::Warn, format!("can't fetch {default}: {e}")));
    }
    run_git(checkout, &["merge", "--ff-only", "--quiet", &default])
        .err()
        .and_then(|e| fail(format!("its {branch} can't fast-forward to {default}, so it has diverged: {e}")))
}

// ── Project ─────────────────────────────────────────────────────────────────────────

/// A skill fleet.toml names: in the checkout's `.claude/skills/`, or the kit's (`~/.claude/skills/`).
fn skill(checkout: &str, name: &str, about: &str, named: Option<&str>, missing: HealthStatus, default: &str) -> HealthCheck {
    let Some(skill) = named else {
        return check("Project Skills", name, about, HealthStatus::Unset, Some(format!("none named: {default}")));
    };
    let homes = [Some(PathBuf::from(checkout)), std::env::var_os("HOME").map(PathBuf::from)];
    let found = homes.iter().flatten().any(|h| h.join(".claude/skills").join(skill).join("SKILL.md").is_file());
    let miss = (!found).then(|| format!("fleet.toml names {skill}, but there's no .claude/skills/{skill}/SKILL.md in the checkout"));
    verdict("Project Skills", name, about, missing, miss)
}

fn worktrees(checkout: &str) -> HealthCheck {
    let about = "no stale worktree entries under review/ or impl/";
    let listing = run_git(checkout, &["worktree", "list", "--porcelain"]).unwrap_or_default();
    let stale: Vec<String> = listing
        .split("\n\n")
        .filter(|entry| entry.lines().any(|l| l.starts_with("prunable")))
        .filter_map(|entry| entry.lines().find_map(|l| l.strip_prefix("worktree ")))
        .map(|p| Path::new(p).file_name().map_or(p.to_string(), |n| n.to_string_lossy().into_owned()))
        .collect();
    let miss = (!stale.is_empty()).then(|| format!("stale: {}, which blocks cutting them again: git -C {checkout} worktree prune", stale.join(", ")));
    verdict("Project Worktrees", "worktrees", about, HealthStatus::Fail, miss)
}

// ── helpers ─────────────────────────────────────────────────────────────────────────

fn run_git(checkout: &str, args: &[&str]) -> std::result::Result<String, String> {
    let out = Command::new("git").arg("-C").arg(checkout).args(args).output().map_err(|e| e.to_string())?;
    let text = |b: &[u8]| String::from_utf8_lossy(b).trim().to_string();
    if out.status.success() { Ok(text(&out.stdout)) } else { Err(crate::preflight::one_line(&out.stderr)) }
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
pub(crate) mod tests {
    use super::*;

    /// A config over a fixture checkout.
    pub(crate) fn cfg(checkout: &str) -> Config {
        let mut c = Config::default();
        c.project.path = checkout.to_string();
        c
    }

    fn status(checks: &[HealthCheck], name: &str) -> (HealthStatus, String) {
        let c = checks.iter().find(|c| c.name == name).unwrap_or_else(|| panic!("no check {name}"));
        (c.status, c.detail.clone().unwrap_or_default())
    }

    #[test]
    fn the_git_checks_pass_once_guarded_and_clean_and_each_miss_is_named() {
        let (dir, checkout, _) = crate::git::fixture::checkout("health", &[]);
        let cfg = cfg(&checkout);
        let git = |args: &[&str]| assert!(Command::new("git").arg("-C").arg(&checkout).args(args).status().unwrap().success());
        let c = checks(&cfg);
        let (s, d) = status(&c, "ref-guard");
        assert!(s == HealthStatus::Fail && d.contains("./install.sh --hooks"), "unguarded: {d}");
        assert_eq!(status(&c, "follows-origin").0, HealthStatus::Unset, "not moved while unguarded");

        std::os::unix::fs::symlink(kit_hooks().join(REF_GUARD), Path::new(&checkout).join(".git/hooks/reference-transaction")).unwrap();
        let c = checks(&cfg);
        for name in ["checkout", "ref-guard", "hooks-path", "git-settings", "follows-origin", "worktrees"] {
            assert_eq!(status(&c, name).0, HealthStatus::Ok, "{name}");
        }

        // Behind origin: the check fast-forwards it.
        let head = |r: &str| String::from_utf8_lossy(&Command::new("git").arg("-C").arg(&checkout).args(["rev-parse", r]).output().unwrap().stdout).trim().to_string();
        git(&["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false", "commit", "-q", "--allow-empty", "-m", "next"]);
        git(&["push", "-q", "origin", "HEAD:refs/heads/master"]);
        let next = head("HEAD");
        git(&["reset", "-q", "--hard", "HEAD~1"]);
        assert_eq!(status(&checks(&cfg), "follows-origin").0, HealthStatus::Ok, "behind is healthy: it follows");
        assert_eq!(head("HEAD"), next, "fast-forwarded to origin/master");

        // An uncommitted change, or another branch: it can't follow, and says so.
        std::fs::write(Path::new(&checkout).join("x"), "x").unwrap();
        git(&["add", "x"]);
        let (s, d) = status(&checks(&cfg), "follows-origin");
        assert!(s == HealthStatus::Fail && d.contains("uncommitted changes"), "{d}");
        git(&["rm", "-q", "--cached", "x"]);
        git(&["switch", "-q", "-c", "other"]);
        let (s, d) = status(&checks(&cfg), "follows-origin");
        assert!(s == HealthStatus::Fail && d.contains("on other, not master"), "{d}");
        git(&["switch", "-q", "master"]);
        assert_eq!(status(&checks(&cfg), "follows-origin").0, HealthStatus::Ok, "back on master");

        // Origin out of reach: a warning, on one line; it holds nothing.
        let origin = run_git(&checkout, &["remote", "get-url", "origin"]).unwrap();
        git(&["remote", "set-url", "origin", "/nowhere/at/all"]);
        let (s, d) = status(&checks(&cfg), "follows-origin");
        assert!(s == HealthStatus::Warn && d.contains("can't fetch origin/master") && !d.contains('\n'), "{d}");
        git(&["remote", "set-url", "origin", &origin]);

        git(&["config", "user.name", "test"]);
        git(&["config", "core.worktree", "/nowhere/at/all"]);
        let c = checks(&cfg);
        let (s, d) = status(&c, "git-settings");
        assert!(s == HealthStatus::Fail && d.contains("user.name = test") && d.contains("core.worktree = /nowhere/at/all"), "found even though it breaks git: {d}");
        assert_eq!(status(&c, "follows-origin").0, HealthStatus::Unset, "never moved with a broken worktree setting");
        assert!(preflight(&cfg).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_named_skill_must_exist_and_only_the_review_skill_holds_the_lookouts() {
        let (dir, checkout, _) = crate::git::fixture::checkout("health-skills", &[]);
        let mut cfg = cfg(&checkout);
        let c = checks(&cfg);
        assert_eq!(status(&c, "review-skill").0, HealthStatus::Unset, "none named: the kit's");

        cfg.review_skill = Some("no-such-review".into());
        cfg.groom_skill = Some("no-such-groom".into());
        let c = checks(&cfg);
        assert_eq!(status(&c, "review-skill").0, HealthStatus::Fail);
        assert_eq!(status(&c, "grooming-skill").0, HealthStatus::Warn, "a grooming skill doesn't hold reviews");
        assert!(problems(&c).iter().any(|p| p.starts_with("review-skill:")) && !problems(&c).iter().any(|p| p.starts_with("grooming-skill:")));

        std::fs::create_dir_all(Path::new(&checkout).join(".claude/skills/no-such-review")).unwrap();
        std::fs::write(Path::new(&checkout).join(".claude/skills/no-such-review/SKILL.md"), "x").unwrap();
        let c = checks(&cfg);
        assert_eq!(status(&c, "review-skill").0, HealthStatus::Ok);
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
