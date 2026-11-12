//! The `glab` Forge driver — shells out to `glab api` (PLAN: reuse the CLI that
//! holds the auth + the platform lore). Fetches open MRs (the armada) and maps them
//! to `ForgeShip`.
//!
//! Reads go through ONE GraphQL query (`MR_FIELDS`): the armada is every open MR exact — pipeline,
//! approvals, open/total threads, auto-merge, merge train, declared blocks — in a call or two, where
//! REST took three or four calls per MR. A single MR (`snapshot`) is the same query by iid. Writes
//! and the review surface stay REST.

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

    /// One GraphQL call (`what` names it in an error): the response, or the errors it reports.
    fn graphql(&self, what: &str, query: &str) -> Result<serde_json::Value> {
        let out = Command::new("glab").args(["api", "graphql", "-f", &format!("query={query}")]).output()?;
        ensure!(out.status.success(), "glab api graphql ({what}) failed: {}", crate::preflight::one_line(&out.stderr));
        let v: serde_json::Value = serde_json::from_slice(&out.stdout)?;
        if let Some(errors) = v.get("errors") {
            anyhow::bail!("glab api graphql ({what}): {errors}");
        }
        Ok(v)
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
    /// The project's MRs matching `filter` (GraphQL `mergeRequests` arguments, the page size
    /// included), every page, each exact. GitLab caps a query's complexity, which grows with the
    /// page size: one MR by iid asks for a page of one.
    fn query_mrs(&self, filter: &str) -> Result<Vec<ForgeShip>> {
        let (mut ships, mut after) = (Vec::new(), String::new());
        loop {
            let query = format!(
                r#"{{ projects(ids: ["gid://gitlab/Project/{}"]) {{ nodes {{ mergeRequests({filter}{after}) {{
                     pageInfo {{ hasNextPage endCursor }} nodes {{ {MR_FIELDS} }} }} }} }} }}"#,
                self.project
            );
            let v = self.graphql("MRs", &query)?;
            let mrs = v
                .pointer("/data/projects/nodes/0/mergeRequests")
                .ok_or_else(|| anyhow!("glab api graphql: no project {}", self.project))?;
            let nodes = mrs.get("nodes").and_then(|n| n.as_array()).into_iter().flatten();
            ships.extend(nodes.filter_map(|n| gql_to_ship(n, &self.ticket_prefix)));
            match mrs.pointer("/pageInfo/endCursor").and_then(|c| c.as_str()) {
                Some(cursor) if mrs.pointer("/pageInfo/hasNextPage").and_then(|h| h.as_bool()) == Some(true) => {
                    after = format!(r#", after: "{cursor}""#);
                }
                _ => return Ok(ships),
            }
        }
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
            let log_excerpt = self
                .api(&format!("projects/{}/jobs/{id}/trace", self.project))
                .map(|log| crate::preflight::log_excerpt(&log))
                .unwrap_or_else(|e| format!("(log unavailable: {e:#})"));
            out.push(FailedJob {
                name: job.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string(),
                url: job.get("web_url").and_then(|u| u.as_str()).unwrap_or("").to_string(),
                log_excerpt,
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

    /// Whether the MR is a draft now, as GitLab has it.
    fn is_draft(&self, iid: i64) -> Result<bool> {
        let mr: serde_json::Value =
            serde_json::from_str(&self.api(&format!("projects/{}/merge_requests/{iid}", self.project))?)?;
        Ok(mr.get("draft").and_then(|x| x.as_bool()).unwrap_or(false))
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

    fn armada_is_exact(&self) -> bool {
        true // one GraphQL query carries every field
    }

    fn armada(&self) -> Result<Vec<ForgeShip>> {
        self.query_mrs("state: opened, first: 100")
    }

    fn snapshot(&self, iid: i64) -> Result<ForgeShip> {
        self.query_mrs(&format!(r#"iids: ["{iid}"], first: 1"#))?.pop().ok_or_else(|| anyhow!("no MR !{iid}"))
    }

    fn review_state(&self, iid: i64) -> Result<ReviewState> {
        let discussions =
            self.api_paginate(&format!("projects/{}/merge_requests/{iid}/discussions", self.project))?;
        Ok(ReviewState {
            open_threads: count_open_threads(&discussions),
            total_threads: count_threads(&discussions),
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
            let armed = is_armed(&v);
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

    fn sync_blocks(&self, child_iid: i64, parents: &[i64]) -> Result<usize> {
        let blocks = format!("projects/{}/merge_requests/{child_iid}/blocks", self.project);
        let (dead, declared) = read_blocks(&serde_json::from_str(&self.api(&blocks)?)?);
        let mut changed = 0;
        for id in dead {
            let out = Command::new("glab").args(["api", "--method", "DELETE", &format!("{blocks}/{id}")]).output()?;
            ensure!(out.status.success(), "unblock !{child_iid} failed: {}", crate::preflight::one_line(&out.stderr));
            changed += 1;
        }
        for &parent in parents.iter().filter(|p| !declared.contains(p)) {
            // The /blocks API takes the parent's GLOBAL id, not its iid.
            let mr: serde_json::Value =
                serde_json::from_str(&self.api(&format!("projects/{}/merge_requests/{parent}", self.project))?)?;
            let gid = mr.get("id").and_then(|i| i.as_i64()).ok_or_else(|| anyhow!("MR !{parent} has no global id"))?;
            let out = Command::new("glab")
                .args(["api", "--method", "POST", &blocks, "-f", &format!("blocking_merge_request_id={gid}")])
                .output()?;
            ensure!(out.status.success(), "block !{child_iid} on !{parent} failed: {}", crate::preflight::one_line(&out.stderr));
            changed += 1;
        }
        Ok(changed)
    }

    fn disarm(&self, iid: i64) -> Result<()> {
        let mr: serde_json::Value =
            serde_json::from_str(&self.api(&format!("projects/{}/merge_requests/{iid}", self.project))?)?;
        if !is_armed(&mr) {
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

    fn describe_mr(&self, iid: i64, title: &str, description: &str) -> Result<()> {
        let title = keep_draft(title, self.is_draft(iid)?);
        let (iid, repo) = (iid.to_string(), self.repo()?);
        let args = ["mr", "update", &iid, "-R", &repo, "--title", &title, "--description", description];
        let out = Command::new("glab").args(args).output()?;
        ensure!(out.status.success(), "glab mr update failed: {}", crate::preflight::one_line(&out.stderr));
        Ok(())
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
        if self.is_draft(iid)? == draft {
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

    fn post_thread(&self, iid: i64, body: &str) -> Result<()> {
        let path = format!("projects/{}/merge_requests/{iid}/discussions", self.project);
        let out = Command::new("glab").args(["api", "--method", "POST", &path, "-f", &format!("body={body}")]).output()?;
        ensure!(out.status.success(), "glab review thread failed: {}", crate::preflight::one_line(&out.stderr));
        Ok(())
    }

    /// The comment, then GitLab's change request (GraphQL, so no pending draft note of the fleet's user
    /// is published with it): the MR's merge waits until that user approves it or lifts the request.
    fn request_changes(&self, iid: i64, body: &str) -> Result<()> {
        self.post_review(iid, body, false)?;
        let query = format!(
            r#"mutation {{ mergeRequestRequestChanges(input: {{ projectPath: "{}", iid: "{iid}" }}) {{ errors }} }}"#,
            self.repo()?
        );
        let v = self.graphql("request changes", &query)?;
        let errors = v.pointer("/data/mergeRequestRequestChanges/errors").and_then(|e| e.as_array());
        ensure!(errors.is_some_and(|e| e.is_empty()), "!{iid}: GitLab refused the change request: {}", v["data"]);
        Ok(())
    }

    fn changed_paths(&self, iids: &[i64]) -> Result<Vec<(i64, Vec<String>)>> {
        let mut all = Vec::new();
        for chunk in iids.chunks(100) {
            let list = chunk.iter().map(|i| format!("\"{i}\"")).collect::<Vec<_>>().join(",");
            let query = format!(
                r#"{{ projects(ids: ["gid://gitlab/Project/{}"]) {{ nodes {{ mergeRequests(iids: [{list}], first: 100) {{
                     nodes {{ iid diffStats {{ path }} }} }} }} }} }}"#,
                self.project
            );
            all.extend(gql_changed_paths(&self.graphql("diffStats", &query)?));
        }
        Ok(all)
    }

    fn file_diff(&self, iid: i64, path: &str) -> Result<String> {
        let files = self.api_paginate(&format!("projects/{}/merge_requests/{iid}/diffs?per_page=100", self.project))?;
        diff_of(&files, path, "diff").ok_or_else(|| anyhow!("!{iid} doesn't change {path}"))
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

/// Every review thread, resolved or not: a discussion with a resolvable, non-system note.
fn count_threads(discussions: &[serde_json::Value]) -> i64 {
    discussions.iter().filter(|d| notes(d).any(|n| nbool(n, "resolvable") && !nbool(n, "system"))).count() as i64
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

/// An MR's `/blocks` split in two: the block ids whose blocking MR closed unmerged (dead — GitLab
/// keeps gating on them), and the iids of the blocking MRs still declared.
fn read_blocks(blocks: &serde_json::Value) -> (Vec<i64>, Vec<i64>) {
    let (mut dead, mut declared) = (Vec::new(), Vec::new());
    for b in blocks.as_array().into_iter().flatten() {
        let Some(parent) = b.get("blocking_merge_request") else { continue };
        if parent.get("state").and_then(|s| s.as_str()) == Some("closed") {
            dead.extend(b.get("id").and_then(|i| i.as_i64()));
        } else {
            declared.extend(parent.get("iid").and_then(|i| i.as_i64()));
        }
    }
    (dead, declared)
}

/// Armed: set to merge on its own once the pipeline passes (GitLab's auto-merge).
fn is_armed(mr: &serde_json::Value) -> bool {
    mr.get("merge_when_pipeline_succeeds").and_then(|x| x.as_bool()).unwrap_or(false)
}

/// What the fleet reads of an MR, as GraphQL selects it. `resolvable - resolved` discussions is the
/// open-thread count (resolvable, unresolved, non-system), matching `count_open_threads` on REST.
const MR_FIELDS: &str = "iid title webUrl sourceBranch targetBranch state draft author { username } diffHeadSha \
    conflicts detailedMergeStatus autoMergeEnabled mergeTrainCar { id } approvalsRequired approvalsLeft \
    resolvableDiscussionsCount resolvedDiscussionsCount headPipeline { status } \
    blockingMergeRequests { visibleMergeRequests { iid state } }";

/// One GraphQL MR node (`MR_FIELDS`) → a ship. GraphQL's enums are upper-case (`OPENED`,
/// `CI_MUST_PASS`, `SUCCESS`); the fleet keeps REST's lower-case vocabulary.
/// Each MR's changed paths, from a `mergeRequests { nodes { iid diffStats { path } } }` answer.
fn gql_changed_paths(v: &serde_json::Value) -> Vec<(i64, Vec<String>)> {
    let nodes = v.pointer("/data/projects/nodes/0/mergeRequests/nodes").and_then(|n| n.as_array());
    nodes
        .into_iter()
        .flatten()
        .filter_map(|n| {
            let iid = n.get("iid")?.as_str()?.parse().ok()?;
            let stats = n.get("diffStats")?.as_array()?;
            Some((iid, stats.iter().filter_map(|s| Some(s.get("path")?.as_str()?.to_string())).collect()))
        })
        .collect()
}

/// One file's diff out of an MR's changed files (GitLab's `/diffs` entries, GitHub's `/files`),
/// matched by its new path or, for a rename or deletion, its old one. `field` names the hunks.
pub(crate) fn diff_of(files: &[serde_json::Value], path: &str, field: &str) -> Option<String> {
    files.iter().find_map(|f| {
        let named = |k: &str| f.get(k).and_then(|p| p.as_str()) == Some(path);
        (named("new_path") || named("old_path") || named("filename") || named("previous_filename"))
            .then(|| f.get(field).and_then(|d| d.as_str()).unwrap_or("").to_string())
    })
}

fn gql_to_ship(m: &serde_json::Value, prefix: &str) -> Option<ForgeShip> {
    let iid = m.get("iid")?.as_str()?.parse().ok()?;
    let s = |k: &str| m.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
    let lower = |p: &str| m.pointer(p).and_then(|v| v.as_str()).map(str::to_lowercase);
    let i = |k: &str| m.get(k).and_then(|v| v.as_i64()).unwrap_or(0);
    let b = |k: &str| m.get(k).and_then(|v| v.as_bool()).unwrap_or(false);
    let (title, source) = (s("title"), s("sourceBranch"));
    let required = i("approvalsRequired");
    let blocked_by = m
        .pointer("/blockingMergeRequests/visibleMergeRequests")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
        .filter(|p| p.get("state").and_then(|s| s.as_str()) != Some("merged"))
        .filter_map(|p| p.get("iid")?.as_str()?.parse().ok())
        .collect();
    Some(ForgeShip {
        iid,
        url: s("webUrl"),
        target: s("targetBranch"),
        ticket: TicketKey::find_in(&source, prefix).or_else(|| TicketKey::find_in(&title, prefix)),
        source,
        pilot: m.pointer("/author/username").and_then(|u| u.as_str()).unwrap_or("").to_string(),
        title,
        state: ship_state(&lower("/state").unwrap_or_default(), b("draft")),
        head_sha: s("diffHeadSha"),
        has_conflicts: b("conflicts"),
        pipeline: lower("/headPipeline/status").as_deref().and_then(pipeline_status),
        open_threads: (i("resolvableDiscussionsCount") - i("resolvedDiscussionsCount")).max(0),
        total_threads: i("resolvableDiscussionsCount"),
        merge_status: lower("/detailedMergeStatus"),
        approvals: Approvals { current: (required - i("approvalsLeft")).max(0), required },
        armed: b("autoMergeEnabled"),
        on_merge_train: m.get("mergeTrainCar").is_some_and(|c| !c.is_null()),
        blocked_by,
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

/// The title to rewrite a draft MR with. GitLab reads draft off the title's prefix, so a new title
/// without one would mark the MR ready; a rewrite changes the words, never the state.
fn keep_draft(title: &str, draft: bool) -> String {
    let marked = ["Draft:", "[Draft]", "(Draft)"].iter().any(|p| title.get(..p.len()).is_some_and(|h| h.eq_ignore_ascii_case(p)));
    if draft && !marked { format!("Draft: {title}") } else { title.to_string() }
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

    // A GraphQL `mergeRequests` node shape (trimmed), from the live API.
    const NODES: &str = r#"[
      {"iid":"2331","title":"PROJ-6187: Remove reliance","webUrl":"https://x/2331","sourceBranch":"feature/PROJ-6187-remove",
       "targetBranch":"master","state":"opened","draft":false,"author":{"username":"Jack.Shannon"},"diffHeadSha":"61b2a8",
       "conflicts":false,"detailedMergeStatus":"CI_MUST_PASS","autoMergeEnabled":true,"mergeTrainCar":null,
       "approvalsRequired":1,"approvalsLeft":0,"resolvableDiscussionsCount":5,"resolvedDiscussionsCount":3,
       "headPipeline":{"status":"SUCCESS"},
       "blockingMergeRequests":{"visibleMergeRequests":[{"iid":"2330","state":"opened"},{"iid":"2200","state":"merged"},{"iid":"2201","state":"closed"}]}},
      {"iid":"2300","title":"chore: no ticket here","webUrl":"https://x/2300","sourceBranch":"chore/cleanup",
       "targetBranch":"master","state":"opened","draft":true,"author":{"username":"me"},"diffHeadSha":"abc",
       "conflicts":true,"detailedMergeStatus":"DRAFT_STATUS","autoMergeEnabled":false,"mergeTrainCar":{"id":"gid://x/1"},
       "approvalsRequired":2,"approvalsLeft":2,"resolvableDiscussionsCount":0,"resolvedDiscussionsCount":0,
       "headPipeline":null,"blockingMergeRequests":{"visibleMergeRequests":[]}}
    ]"#;

    #[test]
    fn changed_paths_and_one_files_diff_read_off_the_forges_answers() {
        let v: serde_json::Value = serde_json::from_str(
            r#"{"data":{"projects":{"nodes":[{"mergeRequests":{"nodes":[
                 {"iid":"2409","diffStats":[{"path":"infra/a.py"},{"path":"docs/b.md"}]},
                 {"iid":"2411","diffStats":[]}]}}]}}}"#,
        )
        .unwrap();
        assert_eq!(
            gql_changed_paths(&v),
            vec![(2409, vec!["infra/a.py".to_string(), "docs/b.md".to_string()]), (2411, vec![])]
        );

        let files: Vec<serde_json::Value> = serde_json::from_str(
            r#"[{"old_path":"a.rs","new_path":"a.rs","diff":"@@ -1 +1 @@\n-x\n+y"},
                {"old_path":"old.rs","new_path":"new.rs","diff":"@@ rename"}]"#,
        )
        .unwrap();
        assert_eq!(diff_of(&files, "a.rs", "diff").as_deref(), Some("@@ -1 +1 @@\n-x\n+y"));
        assert_eq!(diff_of(&files, "old.rs", "diff").as_deref(), Some("@@ rename"), "a rename, by its old path");
        assert_eq!(diff_of(&files, "c.rs", "diff"), None);
    }

    fn ships(prefix: &str) -> Vec<ForgeShip> {
        let v: serde_json::Value = serde_json::from_str(NODES).unwrap();
        v.as_array().unwrap().iter().filter_map(|n| gql_to_ship(n, prefix)).collect()
    }

    #[test]
    fn read_blocks_splits_closed_blockers_from_live_ones() {
        let v: serde_json::Value = serde_json::from_str(
            r#"[{"id":11,"blocking_merge_request":{"iid":2360,"state":"closed"}},
                {"id":12,"blocking_merge_request":{"iid":2361,"state":"opened"}},
                {"id":13,"blocking_merge_request":{"iid":2300,"state":"merged"}}]"#,
        )
        .unwrap();
        assert_eq!(read_blocks(&v), (vec![11], vec![2361, 2300]));
    }

    #[test]
    fn maps_a_graphql_node_exact() {
        let ships = ships("PROJ");
        let a = &ships[0];
        assert_eq!((a.iid, a.ticket.clone(), a.pilot.as_str()), (2331, Some(TicketKey("PROJ-6187".into())), "Jack.Shannon"));
        assert_eq!((a.state, a.head_sha.as_str(), a.has_conflicts), (ShipState::Open, "61b2a8", false));
        assert_eq!(a.merge_status.as_deref(), Some("ci_must_pass"), "upper-case enum → REST's vocabulary");
        assert_eq!(a.pipeline, Some(PipelineStatus::Success));
        assert_eq!((a.open_threads, a.total_threads), (2, 5), "resolvable − resolved are open");
        assert_eq!(a.approvals, Approvals { current: 1, required: 1 });
        assert!(a.armed && !a.on_merge_train);
        assert_eq!(a.blocked_by, vec![2330, 2201], "a merged blocker is settled; a closed one still needs dropping");

        let b = &ships[1];
        assert_eq!((b.ticket.clone(), b.state, b.pipeline), (None, ShipState::Draft, None));
        assert!(b.has_conflicts && !b.armed && b.on_merge_train);
        assert_eq!(b.approvals, Approvals { current: 0, required: 2 });
    }

    /// Live read against a real project: `FLEET_GL_SMOKE=<project id> FLEET_GL_PREFIX=<KEY> cargo test
    /// -p fleetd smoke_reads_the_live_project -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn smoke_reads_the_live_project() {
        let env = |k: &str| std::env::var(k).unwrap_or_else(|_| panic!("{k} unset"));
        let f = GlabForge::new(env("FLEET_GL_SMOKE"), env("FLEET_GL_PREFIX"));
        let ships = f.armada().unwrap();
        for s in &ships {
            println!("!{} {:?} {:?} pipeline={:?} merge={:?} threads={}/{} appr={}/{} armed={} blocked_by={:?}",
                s.iid, s.ticket, s.state, s.pipeline, s.merge_status, s.open_threads, s.total_threads,
                s.approvals.current, s.approvals.required, s.armed, s.blocked_by);
        }
        let first = ships.first().expect("an open MR");
        assert_eq!(&f.snapshot(first.iid).unwrap(), first, "one MR by iid reads the same");
    }

    #[test]
    fn tickets_link_only_by_the_configured_prefix() {
        // Same MRs, another tracker's prefix: nothing links (no cross-project false matches).
        assert!(ships("OTHER").iter().all(|s| s.ticket.is_none()));
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
    fn a_rewrite_keeps_a_draft_a_draft() {
        assert_eq!(keep_draft("PROJ-1: Land it", true), "Draft: PROJ-1: Land it");
        assert_eq!(keep_draft("draft: PROJ-1: Land it", true), "draft: PROJ-1: Land it"); // already marked
        assert_eq!(keep_draft("PROJ-1: Land it", false), "PROJ-1: Land it");
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
}
