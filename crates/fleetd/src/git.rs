//! The `git` worktree driver — reclaims a finished ticket's local worktree (+ its branch) at reap,
//! and berths each lookout in a review worktree. Single surface: `git`. A *ticket's* worktree is
//! the dispatcher's `/fleet-berth` skill; here it's only torn down. A *review* worktree (a detached
//! copy of an MR's head under `review/`, beside the checkout) is cut and struck here, one per
//! lookout. Runs from the project checkout — the main checkout that owns the worktrees.

use anyhow::{anyhow, ensure, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct GitWorktrees {
    project: String,
}

impl GitWorktrees {
    pub fn new(project: impl Into<String>) -> Self {
        Self { project: project.into() }
    }

    /// Preflight: the project checkout exists, is a git work tree, has an `origin` remote, and knows
    /// origin's default branch.
    pub fn preflight(&self) -> Result<String> {
        ensure!(
            Path::new(&self.project).is_dir(),
            "no project checkout at {p} — clone it once: git clone <remote> {p}",
            p = self.project
        );
        crate::preflight::cli("git", &["-C", &self.project, "rev-parse", "--is-inside-work-tree"])?;
        let origin = crate::preflight::cli("git", &["-C", &self.project, "remote", "get-url", "origin"])?;
        Ok(format!("{} (origin {origin}) · default {}", self.project, self.default_branch()?))
    }

    /// origin's default branch as a fork ref (`origin/main`), read from the clone's `origin/HEAD` —
    /// recorded by `git clone` and shared by every ticket worktree, so no forge call is needed.
    pub fn default_branch(&self) -> Result<String> {
        crate::preflight::cli("git", &["-C", &self.project, "symbolic-ref", "--short", "refs/remotes/origin/HEAD"])
            .map_err(|_| {
                anyhow!("origin's default branch is unknown — set it: git -C {} remote set-head origin -a", self.project)
            })
    }

    /// Remove the worktree at `path` and delete its local branch. NEVER `--force`: a dirty worktree
    /// (uncommitted work) makes git refuse — we'd rather keep it and log than nuke unsaved changes.
    /// The branch delete is best-effort (the worktree is already gone; a leftover ref is cheap).
    pub fn remove(&self, path: &str) -> Result<()> {
        self.remove_with(path, false)
    }

    /// Remove the worktree at `path` and its local branch, unsaved work and all: a recalled ticket's
    /// work is forgotten on purpose. What was pushed stays on the forge.
    pub fn discard(&self, path: &str) -> Result<()> {
        self.remove_with(path, true)
    }

    fn remove_with(&self, path: &str, force: bool) -> Result<()> {
        let branch = current_branch(path); // read before we remove it

        let mut args = vec!["-C", self.project.as_str(), "worktree", "remove"];
        if force {
            args.push("--force");
        }
        let out = Command::new("git").args(&args).arg(path).output()?;
        ensure!(
            out.status.success(),
            "git worktree remove {path} failed: {}",
            crate::preflight::one_line(&out.stderr)
        );

        if let Some(b) = branch {
            let _ = Command::new("git").args(["-C", &self.project, "branch", "-D", &b]).output();
        }
        Ok(())
    }

    /// The folder the lookouts' review worktrees live in: `review/`, beside the checkout.
    pub fn reviews(&self) -> PathBuf {
        Path::new(&self.project).parent().unwrap_or(Path::new(".")).join("review")
    }

    /// Berth a lookout: fetch the MR's head and its target, and cut a detached worktree of the head at
    /// `review/<iid>`, with the checkout's shared caches linked in: a real repository at the commit
    /// under review, its base (`origin/<target>`) current, so the lookout needs no fetch of its own.
    /// Detached: no branch, no ref of ours moves. A stale one (a lookout that died) is struck first —
    /// a new head brings a new lookout, never a reused tree.
    pub fn cut_review(&self, iid: i64, fetch_ref: &str, target: &str, head_sha: &str) -> Result<String> {
        let path = self.reviews().join(iid.to_string());
        self.strike_review(&path)?;
        std::fs::create_dir_all(self.reviews())?;
        self.git(&["fetch", "--quiet", "--no-tags", "origin", fetch_ref, &format!("+refs/heads/{target}:refs/remotes/origin/{target}")])?;
        let p = path.to_string_lossy();
        self.git(&["worktree", "add", "--quiet", "--detach", &p, head_sha])?;
        link_shared_caches(Path::new(&self.project), &path);
        Ok(p.into_owned())
    }

    /// Strike a review worktree: `--force`, as it's disposable (a lookout's test run leaves files).
    /// Only ever under `review/` — a path anywhere else (a lookout berthed before review worktrees
    /// existed, in the checkout or a worker's worktree) is left alone.
    pub fn strike_review(&self, path: &Path) -> Result<()> {
        if path.parent() != Some(self.reviews().as_path()) {
            return Ok(());
        }
        if path.exists() {
            let _ = self.git(&["worktree", "remove", "--force", &path.to_string_lossy()]);
            if path.exists() {
                std::fs::remove_dir_all(path).with_context(|| format!("removing {}", path.display()))?;
            }
        }
        self.git(&["worktree", "prune"])
    }

    /// Strike every review worktree no live lookout is berthed in (`live`: their paths) — the ones
    /// left behind while fleetd was down.
    pub fn sweep_reviews(&self, live: &[String]) -> Result<()> {
        let Ok(entries) = std::fs::read_dir(self.reviews()) else { return Ok(()) };
        for path in entries.flatten().map(|e| e.path()) {
            if !live.iter().any(|l| Path::new(l) == path) {
                self.strike_review(&path)?;
            }
        }
        Ok(())
    }

    fn git(&self, args: &[&str]) -> Result<()> {
        let out = Command::new("git").arg("-C").arg(&self.project).args(args).output()?;
        ensure!(out.status.success(), "git {} failed: {}", args.join(" "), crate::preflight::one_line(&out.stderr));
        Ok(())
    }
}

/// Link the checkout's shared caches into a fresh worktree, as `/fleet-berth` does for a ticket's:
/// the dirs the target repo declares in `.claude/settings.json` (`worktree.symlinkDirectories`), so
/// a review doesn't cold-build. Best-effort: a cache the checkout hasn't built yet is skipped.
fn link_shared_caches(checkout: &Path, worktree: &Path) {
    let Ok(settings) = std::fs::read_to_string(checkout.join(".claude/settings.json")) else { return };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&settings) else { return };
    let dirs = json.pointer("/worktree/symlinkDirectories").and_then(|d| d.as_array()).cloned().unwrap_or_default();
    for rel in dirs.iter().filter_map(|d| d.as_str()) {
        let (src, dst) = (checkout.join(rel), worktree.join(rel));
        if src.exists() && !dst.exists() {
            let _ = dst.parent().map(std::fs::create_dir_all);
            let _ = std::os::unix::fs::symlink(&src, &dst);
        }
    }
}

/// The branch checked out in the worktree at `path`; `None` if detached, or not a git work tree.
pub fn current_branch(path: &str) -> Option<String> {
    Command::new("git")
        .args(["-C", path, "branch", "--show-current"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|b| !b.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(dir: &Path, args: &[&str]) {
        assert!(Command::new("git").arg("-C").arg(dir).args(args).output().unwrap().status.success(), "git {args:?}");
    }

    #[test]
    fn default_branch_reads_origin_head_or_says_how_to_set_it() {
        let dir = std::env::temp_dir().join(format!("fleetd-git-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "-q"]);
        let wt = GitWorktrees::new(dir.to_string_lossy());

        let e = wt.default_branch().unwrap_err().to_string();
        assert!(e.contains("remote set-head origin -a"), "{e}");

        git(&dir, &["symbolic-ref", "refs/remotes/origin/HEAD", "refs/remotes/origin/main"]);
        assert_eq!(wt.default_branch().unwrap(), "origin/main");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_sweep_strikes_the_berths_no_live_lookout_holds_and_nothing_outside_review() {
        let (dir, project, sha) = fixture::checkout("sweep", &[1, 2]);
        let wt = GitWorktrees::new(&project);
        let one = wt.cut_review(1, "pull/1/head", "master", &sha).unwrap();
        let two = wt.cut_review(2, "pull/2/head", "master", &sha).unwrap();
        assert_eq!(Path::new(&two), dir.join("review/2"));

        wt.sweep_reviews(std::slice::from_ref(&one)).unwrap();
        assert!(Path::new(&one).exists(), "a live lookout keeps its berth");
        assert!(!Path::new(&two).exists(), "an orphan is struck");

        wt.strike_review(Path::new(&project)).unwrap();
        assert!(Path::new(&project).exists(), "never a path outside review/");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// A real project for the berth tests: a checkout (`<dir>/master`) with a shared `target/` cache
/// declared, whose bare origin carries `pull/<iid>/head` for each iid — what a lookout's berth
/// fetches. Returns the project folder, the checkout, and the heads' SHA.
#[cfg(test)]
pub(crate) mod fixture {
    use std::path::PathBuf;
    use std::process::Command;

    pub(crate) fn checkout(tag: &str, iids: &[i64]) -> (PathBuf, String, String) {
        let dir = std::env::temp_dir().join(format!("fleetd-berth-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let master = dir.join("master");
        std::fs::create_dir_all(master.join(".claude")).unwrap();
        std::fs::create_dir_all(master.join("target")).unwrap();
        std::fs::write(master.join(".claude/settings.json"), r#"{"worktree":{"symlinkDirectories":["target"]}}"#).unwrap();
        let git = |args: &[&str]| {
            let out = Command::new("git").arg("-C").arg(&dir).args(args).output().unwrap();
            assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        let origin = dir.join("origin.git");
        git(&["init", "-q", "--bare", "origin.git"]);
        git(&["-C", "master", "init", "-q"]);
        let id = ["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"];
        git(&[&["-C", "master"], &id[..], &["commit", "-q", "--allow-empty", "-m", "head"]].concat());
        git(&["-C", "master", "remote", "add", "origin", &origin.to_string_lossy()]);
        git(&["-C", "master", "push", "-q", "origin", "HEAD:refs/heads/master"]); // the target a review fetches
        for iid in iids {
            git(&["-C", "master", "push", "-q", "origin", &format!("HEAD:refs/pull/{iid}/head")]);
        }
        let sha = git(&["-C", "master", "rev-parse", "HEAD"]);
        (dir, master.to_string_lossy().into_owned(), sha)
    }
}
