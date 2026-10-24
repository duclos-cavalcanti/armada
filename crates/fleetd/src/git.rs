//! The `git` worktree driver — reclaims a finished ticket's local worktree (+ its branch) at reap.
//! Single surface: `git`. Worktree *creation* is the dispatcher's `/fleet-berth` skill; this is
//! only the teardown. Runs from the hub — the main checkout that owns the worktrees.

use anyhow::{ensure, Result};
use std::process::Command;

pub struct GitWorktrees {
    hub: String,
}

impl GitWorktrees {
    pub fn new(hub: impl Into<String>) -> Self {
        Self { hub: hub.into() }
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
            .args(["-C", &self.hub, "worktree", "remove", path])
            .output()?;
        ensure!(
            out.status.success(),
            "git worktree remove {path} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );

        if let Some(b) = branch {
            let _ = Command::new("git").args(["-C", &self.hub, "branch", "-D", &b]).output();
        }
        Ok(())
    }
}
