//! The `git` worktree driver — reclaims a finished ticket's local worktree (+ its branch) at reap.
//! Single surface: `git`. Worktree *creation* is the dispatcher's `/fleet-berth` skill; this is
//! only the teardown. Runs from the project checkout — the main checkout that owns the worktrees.

use anyhow::{ensure, Result};
use std::path::Path;
use std::process::Command;

pub struct GitWorktrees {
    project: String,
}

impl GitWorktrees {
    pub fn new(project: impl Into<String>) -> Self {
        Self { project: project.into() }
    }

    /// Preflight: the project checkout exists, is a git work tree, and has an `origin` remote.
    pub fn preflight(&self) -> Result<String> {
        ensure!(
            Path::new(&self.project).is_dir(),
            "no project checkout at {p} — clone it once: git clone <remote> {p}",
            p = self.project
        );
        crate::preflight::cli("git", &["-C", &self.project, "rev-parse", "--is-inside-work-tree"])?;
        let origin = crate::preflight::cli("git", &["-C", &self.project, "remote", "get-url", "origin"])?;
        Ok(format!("{} (origin {origin})", self.project))
    }

    /// Remove the worktree at `path` and delete its local branch. NEVER `--force`: a dirty worktree
    /// (uncommitted work) makes git refuse — we'd rather keep it and log than nuke unsaved changes.
    /// The branch delete is best-effort (the worktree is already gone; a leftover ref is cheap).
    pub fn remove(&self, path: &str) -> Result<()> {
        // The branch the worktree is on — read before we remove it.
        let branch = Command::new("git")
            .args(["-C", path, "branch", "--show-current"])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .filter(|b| !b.is_empty());

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
