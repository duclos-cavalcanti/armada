//! The `glab` Forge driver — shells out to `glab api` (PLAN: reuse the CLI that
//! holds the auth + the platform lore). Fetches open MRs (the armada) and maps them
//! to `ForgeShip`.
//!
//! The armada LIST is cheap (core facts only); the single-MR `snapshot` is ENRICHED
//! — it adds `pipeline` + `open_threads` (from the MR detail) and `approvals` (a
//! separate endpoint). `armed` / `on_merge_train` stay defaulted: per #51 they're
//! GraphQL + re-polled at decision time, never bulk-stored-trusted.

use anyhow::{anyhow, ensure, Context, Result};
use domain::ports::Forge;
use domain::{Approvals, CiReport, FailedJob, FloatingNote, ForgeShip, MrDraft, PipelineStatus, ReviewState, ReviewThread, ShipState, TicketKey};
use std::process::Command;

pub struct GlabForge {
    project: String,
    /// The tracker's key prefix — how an MR's branch/title names its ticket.
    ticket_prefix: String,
}

impl GlabForge {
    pub fn new(project: impl Into<String>, ticket_prefix: impl Into<String>) -> Self {
        Self { project: project.into(), ticket_prefix: ticket_prefix.into() }
    }

    fn api(&self, path: &str) -> Result<String> {
        let out = Command::new("glab").args(["api", path]).output()?;
        ensure!(
            out.status.success(),
            "glab api {path} failed: {}",
            crate::preflight::one_line(&out.stderr)
        );
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// `glab api --paginate` for a list endpoint: it emits one JSON array per page,
    /// concatenated, so parse the stream of arrays and flatten into all elements.
    fn api_paginate(&self, path: &str) -> Result<Vec<serde_json::Value>> {
        let out = Command::new("glab").args(["api", "--paginate", path]).output()?;
        ensure!(
            out.status.success(),
            "glab api --paginate {path} failed: {}",
            crate::preflight::one_line(&out.stderr)
        );
        let text = String::from_utf8_lossy(&out.stdout);
        let mut all = Vec::new();
        for page in serde_json::Deserializer::from_str(&text).into_iter::<serde_json::Value>() {
            if let Some(arr) = page?.as_array() {
                all.extend(arr.iter().cloned());
            }
        }
        Ok(all)
    }

    /// The approvals summary (a separate endpoint from the MR body).
    fn approvals(&self, iid: i64) -> Result<Approvals> {
        let json = self.api(&format!("projects/{}/merge_requests/{iid}/approvals", self.project))?;
        let v: serde_json::Value = serde_json::from_str(&json)?;
        let i = |k: &str| v.get(k).and_then(|x| x.as_i64()).unwrap_or(0);
        let (required, left) = (i("approvals_required"), i("approvals_left"));
        Ok(Approvals { current: (required - left).max(0), required })
    }

    /// The MR's latest pipeline: (id, status).
    fn latest_pipeline(&self, iid: i64) -> Result<Option<(i64, String)>> {
        let json = self.api(&format!("projects/{}/merge_requests/{iid}/pipelines", self.project))?;
        let pipelines: serde_json::Value = serde_json::from_str(&json)?;
        Ok(pipelines.as_array().and_then(|a| a.first()).and_then(|p| {
            Some((p.get("id")?.as_i64()?, p.get("status")?.as_str()?.to_string()))
        }))
    }

    /// A pipeline's failed jobs with their log tails, descending into failed child pipelines (the
    /// parent-child layout hides the real failure one level down) — two levels at most.
    fn failed_jobs(&self, pid: i64, depth: u8, out: &mut Vec<FailedJob>) -> Result<()> {
        let jobs: serde_json::Value =
            serde_json::from_str(&self.api(&format!("projects/{}/pipelines/{pid}/jobs?scope[]=failed&per_page=100", self.project))?)?;
        for job in jobs.as_array().into_iter().flatten() {
            let Some(id) = job.get("id").and_then(|i| i.as_i64()) else { continue };
            let log_tail = self
                .api(&format!("projects/{}/jobs/{id}/trace", self.project))
                .map(|log| crate::preflight::log_tail(&log, 80))
                .unwrap_or_else(|e| format!("(log unavailable: {e:#})"));
            out.push(FailedJob {
                name: job.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string(),
                url: job.get("web_url").and_then(|u| u.as_str()).unwrap_or("").to_string(),
                log_tail,
            });
        }
        if depth < 2 {
            let bridges: serde_json::Value =
                serde_json::from_str(&self.api(&format!("projects/{}/pipelines/{pid}/bridges", self.project))?)?;
            for child in failed_children(&bridges) {
                self.failed_jobs(child, depth + 1, out)?;
            }
        }
        Ok(())
    }

    /// Cancel an armed MR's merge-when-pipeline-succeeds (callers check it's armed first).
    fn cancel_auto_merge(&self, iid: i64) -> Result<()> {
        let path = format!("projects/{}/merge_requests/{iid}/cancel_merge_when_pipeline_succeeds", self.project);
        let out = Command::new("glab").args(["api", "--method", "POST", &path]).output()?;
        ensure!(out.status.success(), "glab cancel auto-merge failed: {}", crate::preflight::one_line(&out.stderr));
        Ok(())
    }

    /// The OWNER/REPO path of the configured project — `glab mr` SUBCOMMANDS need it via `-R`.
    /// Unlike `glab api projects/{id}/…` (which pins the project in the REST path), `glab mr <verb>`
    /// resolves its repo from `-R` or the ambient CWD git remote — so without this it shells against
    /// fleetd's own checkout (the kit repo), not the forge target, and 404s. Resolved from the id.
    fn repo(&self) -> Result<String> {
        let json = self.api(&format!("projects/{}", self.project))?;
        let v: serde_json::Value = serde_json::from_str(&json)?;
        v.get("path_with_namespace")
            .and_then(|x| x.as_str())
            .map(String::from)
            .ok_or_else(|| anyhow!("forge: project {} has no path_with_namespace", self.project))
    }
}

impl Forge for GlabForge {
    fn preflight(&self) -> Result<String> {
        crate::preflight::cli("glab", &["auth", "status"])?; // installed + logged in
        let json = crate::preflight::cli("glab", &["api", &format!("projects/{}", self.project)])?;
        let v: serde_json::Value = serde_json::from_str(&json)?;
        let path = v.get("path_with_namespace").and_then(|p| p.as_str()).unwrap_or(&self.project);
        Ok(format!("glab logged in · project {path}"))
    }

    fn stacks(&self) -> bool {
        true // MR dependencies via /blocks
    }

    fn armada(&self) -> Result<Vec<ForgeShip>> {
        let json = self.api(&format!(
            "projects/{}/merge_requests?state=opened&per_page=100",
            self.project
        ))?;
        parse_glab_mrs(&json, &self.ticket_prefix)
    }

    fn snapshot(&self, iid: i64) -> Result<ForgeShip> {
        let json = self.api(&format!("projects/{}/merge_requests/{iid}", self.project))?;
        let v: serde_json::Value = serde_json::from_str(&json)?;
        let mut ship = mr_to_ship(&v, &self.ticket_prefix).context("could not map MR JSON to a ship")?;
        ship.approvals = self.approvals(iid)?; // separate endpoint, not on the MR body
        Ok(ship)
    }

    fn review_state(&self, iid: i64) -> Result<ReviewState> {
        let discussions =
            self.api_paginate(&format!("projects/{}/merge_requests/{iid}/discussions", self.project))?;
        Ok(ReviewState {
            open_threads: count_open_threads(&discussions),
            threads: open_threads(&discussions),
            floating_notes: floating_notes(&discussions),
        })
    }

    fn reply(&self, iid: i64, thread: &str, body: &str, resolve: bool) -> Result<()> {
        let discussion = format!("projects/{}/merge_requests/{iid}/discussions/{thread}", self.project);
        let note = format!("{discussion}/notes");
        let out = Command::new("glab").args(["api", "--method", "POST", &note, "-f", &format!("body={body}")]).output()?;
        ensure!(out.status.success(), "glab reply failed: {}", crate::preflight::one_line(&out.stderr));
        if resolve {
            let out = Command::new("glab").args(["api", "--method", "PUT", &discussion, "-f", "resolved=true"]).output()?;
            ensure!(out.status.success(), "glab resolve failed: {}", crate::preflight::one_line(&out.stderr));
        }
        Ok(())
    }

    fn rebase(&self, iid: i64) -> Result<()> {
        // Disarm MWPS first on an armed+conflicted MR: a merge train can't resolve a conflict (it
        // evicts the car), so cancel the arm before the rebase/resolve path runs. Best-effort.
        if let Ok(v) = self
            .api(&format!("projects/{}/merge_requests/{iid}", self.project))
            .and_then(|j| Ok(serde_json::from_str::<serde_json::Value>(&j)?))
        {
            let armed = v.get("merge_when_pipeline_succeeds").and_then(|x| x.as_bool()).unwrap_or(false);
            let conflicted = v.get("has_conflicts").and_then(|x| x.as_bool()).unwrap_or(false)
                || v.get("detailed_merge_status").and_then(|x| x.as_str()) == Some("conflict");
            if armed && conflicted {
                let _ = self.cancel_auto_merge(iid);
            }
        }
        let out = Command::new("glab")
            .args([
                "api",
                "--method",
                "PUT",
                &format!("projects/{}/merge_requests/{iid}/rebase", self.project),
            ])
            .output()?;
        ensure!(out.status.success(), "glab rebase failed: {}", crate::preflight::one_line(&out.stderr));
        Ok(())
    }

    fn block_on(&self, child_iid: i64, parent_iid: i64) -> Result<()> {
        // Idempotent: skip if `parent` is already a declared blocker of `child`.
        let blocks = self.api(&format!("projects/{}/merge_requests/{child_iid}/blocks", self.project))?;
        let already = serde_json::from_str::<serde_json::Value>(&blocks)
            .ok()
            .and_then(|v| {
                v.as_array().map(|a| {
                    a.iter().any(|b| {
                        b.get("blocking_merge_request").and_then(|m| m.get("iid")).and_then(|i| i.as_i64())
                            == Some(parent_iid)
                    })
                })
            })
            .unwrap_or(false);
        if already {
            return Ok(());
        }
        // The /blocks API takes the parent's GLOBAL id, not its iid.
        let parent = self.api(&format!("projects/{}/merge_requests/{parent_iid}", self.project))?;
        let gid = serde_json::from_str::<serde_json::Value>(&parent)?
            .get("id")
            .and_then(|i| i.as_i64())
            .ok_or_else(|| anyhow!("block_on: parent MR !{parent_iid} has no global id"))?;
        let out = Command::new("glab")
            .args([
                "api",
                "--method",
                "POST",
                &format!("projects/{}/merge_requests/{child_iid}/blocks", self.project),
                "-f",
                &format!("blocking_merge_request_id={gid}"),
            ])
            .output()?;
        ensure!(out.status.success(), "block_on failed: {}", crate::preflight::one_line(&out.stderr));
        Ok(())
    }

    fn disarm(&self, iid: i64) -> Result<()> {
        let mr: serde_json::Value =
            serde_json::from_str(&self.api(&format!("projects/{}/merge_requests/{iid}", self.project))?)?;
        if !mr.get("merge_when_pipeline_succeeds").and_then(|x| x.as_bool()).unwrap_or(false) {
            return Ok(()); // not armed — GitLab would 406 the cancel
        }
        self.cancel_auto_merge(iid)
    }

    fn open_mr(&self, d: &MrDraft) -> Result<String> {
        let me: serde_json::Value = serde_json::from_str(&self.api("user")?)?;
        let me = me.get("username").and_then(|u| u.as_str()).context("glab api user: no username")?;
        let repo = self.repo()?;
        // `--head` too: glab otherwise takes the SOURCE project from the git remote of fleetd's own
        // cwd (the kit repo), and GitLab refuses an MR from a project that isn't a fork of the
        // target. A fleet MR is always same-project.
        let mut args = vec![
            "mr", "create", "-R", &repo, "--head", &repo,
            "--source-branch", &d.source, "--target-branch", &d.target,
            "--title", &d.title, "--description", &d.description,
            "--assignee", me, "--squash-before-merge", "--yes",
        ];
        if d.draft {
            args.push("--draft");
        }
        let out = Command::new("glab").args(&args).output()?;
        ensure!(out.status.success(), "glab mr create failed: {}", crate::preflight::one_line(&out.stderr));
        let stdout = String::from_utf8_lossy(&out.stdout);
        Ok(stdout.lines().rev().find(|l| l.trim_start().starts_with("http")).unwrap_or(stdout.trim()).trim().to_string())
    }

    fn close_mr(&self, iid: i64) -> Result<()> {
        let mr: serde_json::Value =
            serde_json::from_str(&self.api(&format!("projects/{}/merge_requests/{iid}", self.project))?)?;
        if mr.get("state").and_then(|s| s.as_str()) == Some("closed") {
            return Ok(()); // already closed
        }
        let out = Command::new("glab").args(["mr", "close", &iid.to_string(), "-R", &self.repo()?]).output()?;
        ensure!(out.status.success(), "glab mr close failed: {}", crate::preflight::one_line(&out.stderr));
        Ok(())
    }

    fn set_draft(&self, iid: i64, draft: bool) -> Result<()> {
        let mr: serde_json::Value =
            serde_json::from_str(&self.api(&format!("projects/{}/merge_requests/{iid}", self.project))?)?;
        if mr.get("draft").and_then(|x| x.as_bool()).unwrap_or(false) == draft {
            return Ok(()); // already there
        }
        let flag = if draft { "--draft" } else { "--ready" };
        let out = Command::new("glab").args(["mr", "update", &iid.to_string(), "-R", &self.repo()?, flag]).output()?;
        ensure!(out.status.success(), "glab mr update {flag} failed: {}", crate::preflight::one_line(&out.stderr));
        Ok(())
    }

    fn ci(&self, iid: i64) -> Result<CiReport> {
        let Some((pid, status)) = self.latest_pipeline(iid)? else {
            return Ok(CiReport { status: None, failed: vec![] });
        };
        let mut failed = Vec::new();
        self.failed_jobs(pid, 0, &mut failed)?;
        Ok(CiReport { status: pipeline_status(&status), failed })
    }

    fn fetch_ref(&self, iid: i64) -> String {
        format!("merge-requests/{iid}/head")
    }

    fn post_review(&self, iid: i64, body: &str, approve: bool) -> Result<()> {
        let notes = format!("projects/{}/merge_requests/{iid}/notes", self.project);
        let out = Command::new("glab").args(["api", "--method", "POST", &notes, "-f", &format!("body={body}")]).output()?;
        ensure!(out.status.success(), "glab review note failed: {}", crate::preflight::one_line(&out.stderr));
        if approve {
            let path = format!("projects/{}/merge_requests/{iid}/approve", self.project);
            let out = Command::new("glab").args(["api", "--method", "POST", &path]).output()?;
            ensure!(out.status.success(), "glab approve failed: {}", crate::preflight::one_line(&out.stderr));
        }
        Ok(())
    }

    fn retry_ci(&self, iid: i64) -> Result<()> {
        let (pid, _) = self.latest_pipeline(iid)?.ok_or_else(|| anyhow!("!{iid}: no pipeline to retry"))?;
        let path = format!("projects/{}/pipelines/{pid}/retry", self.project);
        let out = Command::new("glab").args(["api", "--method", "POST", &path]).output()?;
        ensure!(out.status.success(), "glab pipeline retry failed: {}", crate::preflight::one_line(&out.stderr));
        Ok(())
    }

    fn arm_merge(&self, iid: i64) -> Result<()> {
        let repo = self.repo()?;
        let out = Command::new("glab")
            .args(["mr", "merge", &iid.to_string(), "-R", &repo, "--auto-merge", "--yes"])
            .output()?;
        ensure!(out.status.success(), "glab arm-merge failed: {}", crate::preflight::one_line(&out.stderr));
        Ok(())
    }

    fn request_review(&self, ticket: &TicketKey) -> Result<Vec<String>> {
        // Resolve the ticket's open MR from the armada (its branch/title carries the ticket key) —
        // the MR exists on the forge even if our sync hasn't picked it up yet.
        let ship = self
            .armada()?
            .into_iter()
            .find(|s| s.ticket.as_ref() == Some(ticket) && matches!(s.state, ShipState::Open | ShipState::Draft))
            .ok_or_else(|| anyhow!("request_review {}: no open MR for the ticket", ticket.0))?;
        self.set_draft(ship.iid, false)?; // review starts at ready

        // Team = the approval rules' eligible approvers (the server-side "team"), unique, minus the
        // author (GitLab rejects the author as their own reviewer).
        let rules = self.api(&format!("projects/{}/approval_rules", self.project))?;
        let reviewers = team_from_rules(&rules, &ship.pilot);
        ensure!(!reviewers.is_empty(), "request_review {}: no eligible approvers resolved", ticket.0);

        let repo = self.repo()?;
        let out = Command::new("glab")
            .args(["mr", "update", &ship.iid.to_string(), "-R", &repo, "--reviewer", &reviewers.join(",")])
            .output()?;
        ensure!(
            out.status.success(),
            "glab mr update --reviewer failed: {}",
            crate::preflight::one_line(&out.stderr)
        );
        Ok(reviewers)
    }
}

/// The reviewer set from the approval rules: every rule's `eligible_approvers`, de-duplicated and
/// sorted, minus the author. Vendor→domain mapping stays in the adapter (ported from the bash
/// `glab/mr-request-review.sh`).
fn team_from_rules(json: &str, author: &str) -> Vec<String> {
    let v: serde_json::Value = serde_json::from_str(json).unwrap_or(serde_json::Value::Null);
    let mut team = std::collections::BTreeSet::new();
    for rule in v.as_array().into_iter().flatten() {
        for approver in rule.get("eligible_approvers").and_then(|a| a.as_array()).into_iter().flatten() {
            if let Some(name) = approver.get("username").and_then(|n| n.as_str()) {
                if name != author {
                    team.insert(name.to_string());
                }
            }
        }
    }
    team.into_iter().collect()
}

// ── review surface: the two discussion filters (ported from mr-open-threads / mr-floating-notes) ──

/// The merge-blocking thread count: discussions with ≥1 note that is resolvable AND not resolved
/// AND not a system note. Structural (by thread state), never by scanning bodies — the discipline
/// that kept the bash count-gate false-positive-free.
fn count_open_threads(discussions: &[serde_json::Value]) -> i64 {
    discussions
        .iter()
        .filter(|d| notes(d).any(|n| nbool(n, "resolvable") && !nbool(n, "resolved") && !nbool(n, "system")))
        .count() as i64
}

/// The open threads themselves (the same predicate as the count): each discussion with an
/// unresolved resolvable note, pointed at by its opening note.
fn open_threads(discussions: &[serde_json::Value]) -> Vec<ReviewThread> {
    discussions
        .iter()
        .filter(|d| notes(d).any(|n| nbool(n, "resolvable") && !nbool(n, "resolved") && !nbool(n, "system")))
        .filter_map(|d| {
            let first = notes(d).next()?;
            Some(ReviewThread {
                id: d.get("id")?.as_str()?.to_string(),
                path: first.pointer("/position/new_path").and_then(|p| p.as_str()).map(String::from),
                line: first.pointer("/position/new_line").and_then(|l| l.as_i64()),
                author: first.pointer("/author/username").and_then(|u| u.as_str()).map(String::from),
                body: first.get("body").and_then(|b| b.as_str()).unwrap_or("").to_string(),
            })
        })
        .collect()
}

/// The floating notes the count EXCLUDES: notes that are not resolvable and not system (a reviewer
/// HOLD / `/mr-review` summary that gates approval, not merge). Content only — judgment is the caller's.
fn floating_notes(discussions: &[serde_json::Value]) -> Vec<FloatingNote> {
    discussions
        .iter()
        .flat_map(notes)
        .filter(|n| !nbool(n, "resolvable") && !nbool(n, "system"))
        .map(|n| FloatingNote {
            id: n.get("id").and_then(|v| v.as_i64()).unwrap_or(0),
            author: n.get("author").and_then(|a| a.get("username")).and_then(|u| u.as_str()).map(String::from),
            created_at: n.get("created_at").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            body: n.get("body").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        })
        .collect()
}

/// A discussion's notes (empty when absent/malformed).
fn notes(d: &serde_json::Value) -> impl Iterator<Item = &serde_json::Value> {
    d.get("notes").and_then(|n| n.as_array()).into_iter().flatten()
}

/// A note's boolean flag, defaulting false (matches the jq `// false`).
fn nbool(n: &serde_json::Value, key: &str) -> bool {
    n.get(key).and_then(|v| v.as_bool()).unwrap_or(false)
}

fn parse_glab_mrs(json: &str, prefix: &str) -> Result<Vec<ForgeShip>> {
    let v: serde_json::Value = serde_json::from_str(json)?;
    let arr = v.as_array().context("expected a JSON array of MRs from glab")?;
    Ok(arr.iter().filter_map(|m| mr_to_ship(m, prefix)).collect())
}

fn mr_to_ship(m: &serde_json::Value, prefix: &str) -> Option<ForgeShip> {
    let iid = m.get("iid")?.as_i64()?;
    let s = |k: &str| m.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
    let title = s("title");
    let source = m.get("source_branch").and_then(|v| v.as_str()).unwrap_or("");
    let draft = m.get("draft").and_then(|v| v.as_bool()).unwrap_or(false);
    let gl_state = m.get("state").and_then(|v| v.as_str()).unwrap_or("opened");
    let pilot = m
        .get("author")
        .and_then(|a| a.get("username"))
        .and_then(|u| u.as_str())
        .unwrap_or("")
        .to_string();
    Some(ForgeShip {
        iid,
        url: s("web_url"),
        target: m.get("target_branch").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
        source: source.to_string(),
        pilot,
        ticket: TicketKey::find_in(source, prefix).or_else(|| TicketKey::find_in(&title, prefix)),
        title,
        state: ship_state(gl_state, draft),
        head_sha: s("sha"),
        has_conflicts: m.get("has_conflicts").and_then(|v| v.as_bool()).unwrap_or(false),
        // Present only on the single-MR DETAIL (absent on the list → these default):
        pipeline: m
            .get("head_pipeline")
            .and_then(|p| p.get("status"))
            .and_then(|s| s.as_str())
            .and_then(pipeline_status),
        // coarse: blocking threads unresolved → 1, else 0 (exact count is decision-time).
        open_threads: match m.get("blocking_discussions_resolved").and_then(|v| v.as_bool()) {
            Some(false) => 1,
            _ => 0,
        },
        // GitLab's authoritative merge-readiness — the sailor's land-decision signal.
        merge_status: m.get("detailed_merge_status").and_then(|v| v.as_str()).map(String::from),
        // `approvals` filled by `snapshot` (separate endpoint); `armed`/`on_merge_train`
        // are GraphQL + decision-time per #51 — never bulk-stored here.
        approvals: Approvals { current: 0, required: 0 },
        armed: false,
        on_merge_train: false,
    })
}

/// The downstream pipelines of a pipeline's failed bridge (trigger) jobs.
fn failed_children(bridges: &serde_json::Value) -> Vec<i64> {
    bridges
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|b| b.get("downstream_pipeline"))
        .filter(|d| d.get("status").and_then(|s| s.as_str()) == Some("failed"))
        .filter_map(|d| d.get("id").and_then(|i| i.as_i64()))
        .collect()
}

/// Map GitLab's pipeline vocabulary onto our five states (vendor→domain stays in the
/// adapter). In-flight variants collapse to `Running`; `skipped`/unknown → `None`.
fn pipeline_status(s: &str) -> Option<PipelineStatus> {
    match s {
        "success" => Some(PipelineStatus::Success),
        "failed" => Some(PipelineStatus::Failed),
        "canceled" | "cancelled" => Some(PipelineStatus::Canceled),
        "manual" => Some(PipelineStatus::Manual),
        "running" | "pending" | "created" | "preparing" | "scheduled" | "waiting_for_resource" => {
            Some(PipelineStatus::Running)
        }
        _ => None,
    }
}

fn ship_state(gitlab_state: &str, draft: bool) -> ShipState {
    match gitlab_state {
        "merged" => ShipState::Merged,
        "closed" | "locked" => ShipState::Closed,
        _ if draft => ShipState::Draft,
        _ => ShipState::Open,
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    // Real glab field shape (trimmed), from the live API.
    const SAMPLE: &str = r#"[
      {"iid":2331,"title":"PROJ-6187: Remove reliance","source_branch":"feature/PROJ-6187-remove",
       "target_branch":"master","web_url":"https://x/2331","draft":false,"state":"opened",
       "sha":"61b2a8","detailed_merge_status":"ci_must_pass","has_conflicts":false,
       "author":{"username":"Jack.Shannon"}},
      {"iid":2300,"title":"chore: no ticket here","source_branch":"chore/cleanup",
       "target_branch":"master","web_url":"https://x/2300","draft":true,"state":"opened",
       "sha":"abc","has_conflicts":true,"author":{"username":"me"}}
    ]"#;

    #[test]
    fn parses_core_fields() {
        let ships = parse_glab_mrs(SAMPLE, "PROJ").unwrap();
        assert_eq!(ships.len(), 2);

        let a = &ships[0];
        assert_eq!(a.iid, 2331);
        assert_eq!(a.ticket, Some(TicketKey("PROJ-6187".into())));
        assert_eq!(a.pilot, "Jack.Shannon");
        assert_eq!(a.target, "master");
        assert_eq!(a.state, ShipState::Open);
        assert_eq!(a.head_sha, "61b2a8");
        assert!(!a.has_conflicts);
        assert_eq!(a.merge_status.as_deref(), Some("ci_must_pass")); // detailed_merge_status captured

        let b = &ships[1];
        assert_eq!(b.ticket, None); // no ticket key in branch/title
        assert_eq!(b.state, ShipState::Draft); // draft=true
        assert!(b.has_conflicts);
    }

    #[test]
    fn tickets_link_only_by_the_configured_prefix() {
        // Same MRs, another tracker's prefix: nothing links (no cross-project false matches).
        assert!(parse_glab_mrs(SAMPLE, "OTHER").unwrap().iter().all(|s| s.ticket.is_none()));
    }

    #[test]
    fn follows_only_failed_child_pipelines() {
        let bridges = serde_json::json!([
            {"name": "quality", "downstream_pipeline": {"id": 11, "status": "failed"}},
            {"name": "security", "downstream_pipeline": {"id": 12, "status": "success"}},
            {"name": "pending", "downstream_pipeline": null}
        ]);
        assert_eq!(failed_children(&bridges), vec![11]);
    }

    #[test]
    fn ship_state_mapping() {
        assert_eq!(ship_state("opened", false), ShipState::Open);
        assert_eq!(ship_state("opened", true), ShipState::Draft);
        assert_eq!(ship_state("merged", false), ShipState::Merged);
        assert_eq!(ship_state("closed", false), ShipState::Closed);
    }

    #[test]
    fn pipeline_vocab_maps_to_domain() {
        assert_eq!(pipeline_status("success"), Some(PipelineStatus::Success));
        assert_eq!(pipeline_status("pending"), Some(PipelineStatus::Running)); // in-flight
        assert_eq!(pipeline_status("skipped"), None);
    }

    #[test]
    fn team_from_rules_unions_eligible_approvers_minus_author() {
        let rules = r#"[
          {"name":"team","eligible_approvers":[{"username":"alice"},{"username":"bob"},{"username":"me"}]},
          {"name":"security","eligible_approvers":[{"username":"bob"},{"username":"carol"}]}
        ]"#;
        // union {alice,bob,carol,me}, sorted, minus the author "me".
        assert_eq!(team_from_rules(rules, "me"), vec!["alice", "bob", "carol"]);
        // no author to drop → the full de-duped set.
        assert_eq!(team_from_rules(rules, "nobody"), vec!["alice", "bob", "carol", "me"]);
        // malformed → empty (caller errors on empty).
        assert!(team_from_rules("null", "me").is_empty());
    }

    #[test]
    fn review_filters_split_blocking_threads_from_floating_notes() {
        let discussions: Vec<serde_json::Value> = serde_json::from_str(
            r#"[
              {"id":"d1","notes":[{"id":1,"resolvable":true,"resolved":false,"system":false,"author":{"username":"rev"},"created_at":"t1","body":"fix this","position":{"new_path":"src/a.rs","new_line":7}}]},
              {"notes":[{"id":2,"resolvable":true,"resolved":true,"system":false,"author":{"username":"rev"},"created_at":"t2","body":"done"}]},
              {"notes":[{"id":3,"system":true,"author":{"username":"gitlab"},"created_at":"t3","body":"added 2 commits"}]},
              {"notes":[{"id":4,"resolvable":false,"system":false,"author":{"username":"rev"},"created_at":"t4","body":"HOLD: must-fix"}]}
            ]"#,
        )
        .unwrap();

        // Only the resolvable-AND-unresolved-AND-not-system thread blocks the merge.
        assert_eq!(count_open_threads(&discussions), 1);
        let threads = open_threads(&discussions);
        assert_eq!(threads.len(), 1, "the thread list matches the count");
        let t = &threads[0];
        assert_eq!((t.id.as_str(), t.path.as_deref(), t.line, t.body.as_str()), ("d1", Some("src/a.rs"), Some(7), "fix this"));

        // The non-resolvable, non-system note floats; the system note belongs to neither surface.
        let floating = floating_notes(&discussions);
        assert_eq!(floating.len(), 1);
        assert_eq!(floating[0].id, 4);
        assert_eq!(floating[0].author.as_deref(), Some("rev"));
        assert_eq!(floating[0].body, "HOLD: must-fix");
    }

    #[test]
    fn detail_enriches_pipeline_and_threads() {
        // The single-MR detail carries head_pipeline + blocking_discussions_resolved.
        let detail = r#"{"iid":2338,"title":"PROJ-6164: x","source_branch":"chore/PROJ-6164-y",
          "target_branch":"master","web_url":"https://x/2338","draft":false,"state":"opened",
          "sha":"0e31","has_conflicts":false,"author":{"username":"me"},
          "head_pipeline":{"status":"success"},"blocking_discussions_resolved":false}"#;
        let v: serde_json::Value = serde_json::from_str(detail).unwrap();
        let ship = mr_to_ship(&v, "PROJ").unwrap();
        assert_eq!(ship.pipeline, Some(PipelineStatus::Success));
        assert_eq!(ship.open_threads, 1); // unresolved blocking threads

        // The list shape (no pipeline/threads) still defaults cleanly.
        assert_eq!(parse_glab_mrs(SAMPLE, "PROJ").unwrap()[0].pipeline, None);
    }
}
