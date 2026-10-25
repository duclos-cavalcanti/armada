//! The `git` worktree driver — reclaims a finished ticket's local worktree (+ its branch) at reap.
//! Single surface: `git`. Worktree *creation* is the dispatcher's `/fleet-berth` skill; this is
//! only the teardown. Runs from the project checkout — the main checkout that owns the worktrees.

use anyhow::{anyhow, ensure, Result};
use std::path::Path;
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
        let branch = current_branch(path); // read before we remove it

        let out = Command::new("git")
            .args(["-C", &self.project, "worktree", "remove", path])
            .output()?;
        ensure!(
            out.status.success(),
            "git worktree remove {path} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );

        if let Some(b) = branch {
            let _ = Command::new("git").args(["-C", &self.project, "branch", "-D", &b]).output();
        }
        Ok(())
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
}
