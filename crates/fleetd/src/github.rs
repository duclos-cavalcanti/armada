//! The `gh` Forge driver — GitHub through its CLI (which holds the auth). A pull request is a ship.
//!
//! Two GitHub facts shape it: there are no MR dependencies, so this forge doesn't stack
//! (`stacks() = false`, and `block_on` is refused); and GitHub requests reviewers itself (CODEOWNERS,
//! when a PR turns ready), so `request_review` only marks the PR ready. GitHub's merge-readiness
//! (`mergeStateStatus`) is translated into GitLab's `merge_status` words the sailor already reads.

use anyhow::{anyhow, bail, ensure, Context, Result};
use domain::ports::Forge;
use domain::{Approvals, FloatingNote, ForgeShip, MrDraft, PipelineStatus, ReviewState, ShipState, TicketKey};
use serde_json::Value;
use std::process::Command;

/// The PR fields every ship mapping reads (`gh pr list` / `gh pr view --json`).
const FIELDS: &str = "number,url,baseRefName,headRefName,author,title,isDraft,state,headRefOid,\
                      mergeable,mergeStateStatus,reviewDecision,statusCheckRollup,autoMergeRequest,latestReviews";

pub struct GhForge {
    /// `owner/repo`.
    repo: String,
    /// The tracker's key prefix — how a PR's branch/title names its ticket.
    ticket_prefix: String,
}

impl GhForge {
    pub fn new(repo: impl Into<String>, ticket_prefix: impl Into<String>) -> Self {
        Self { repo: repo.into(), ticket_prefix: ticket_prefix.into() }
    }

    /// Run `gh <args> -R <repo>` and return its stdout.
    fn gh(&self, args: &[&str]) -> Result<String> {
        let out = Command::new("gh").args(args).args(["-R", &self.repo]).output()?;
        ensure!(
            out.status.success(),
            "gh {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// One PR's JSON (the `FIELDS` set).
    fn view(&self, n: i64) -> Result<Value> {
        Ok(serde_json::from_str(&self.gh(&["pr", "view", &n.to_string(), "--json", FIELDS])?)?)
    }

    fn owner_name(&self) -> (&str, &str) {
        self.repo.split_once('/').unwrap_or((&self.repo, ""))
    }
}

impl Forge for GhForge {
    fn preflight(&self) -> Result<String> {
        crate::preflight::cli("gh", &["auth", "status"])?; // installed + logged in
        let repo: Value = serde_json::from_str(&crate::preflight::cli("gh", &["api", &format!("repos/{}", self.repo)])?)?;
        let full = repo.get("full_name").and_then(Value::as_str).unwrap_or(&self.repo);
        let mut line = format!("gh logged in · repo {full}");
        // Warnings, not failures: a deployment may run auto_merge = false, or accept an unprotected branch.
        if !repo.get("allow_auto_merge").and_then(Value::as_bool).unwrap_or(false) {
            line.push_str(" · ⚠ auto-merge is off in the repo settings (the sailor can't arm)");
        }
        let default = repo.get("default_branch").and_then(Value::as_str).unwrap_or("main");
        let checks = format!("repos/{}/branches/{default}/protection/required_status_checks", self.repo);
        if crate::preflight::cli("gh", &["api", &checks]).is_err() {
            line.push_str(&format!(" · ⚠ {default} has no required checks (an armed PR merges without waiting for CI)"));
        }
        Ok(line)
    }

    fn stacks(&self) -> bool {
        false // no MR dependencies on GitHub
    }

    fn armada(&self) -> Result<Vec<ForgeShip>> {
        let json = self.gh(&["pr", "list", "--state", "open", "--limit", "100", "--json", FIELDS])?;
        let prs: Vec<Value> = serde_json::from_str(&json)?;
        Ok(prs.iter().filter_map(|p| pr_to_ship(p, &self.ticket_prefix)).collect())
    }

    fn snapshot(&self, n: i64) -> Result<ForgeShip> {
        let mut ship = pr_to_ship(&self.view(n)?, &self.ticket_prefix).context("could not map PR JSON to a ship")?;
        ship.open_threads = self.review_state(n)?.open_threads;
        Ok(ship)
    }

    fn review_state(&self, n: i64) -> Result<ReviewState> {
        let (owner, name) = self.owner_name();
        let query = "query($o:String!,$n:String!,$p:Int!){repository(owner:$o,name:$n){pullRequest(number:$p){\
                     reviewThreads(first:100){nodes{isResolved}}\
                     comments(first:100){nodes{databaseId author{login} createdAt body}}\
                     reviews(first:100){nodes{databaseId author{login} createdAt body}}}}}";
        let out = Command::new("gh")
            .args(["api", "graphql", "-f", &format!("query={query}")])
            .args(["-f", &format!("o={owner}"), "-f", &format!("n={name}"), "-F", &format!("p={n}")])
            .output()?;
        ensure!(out.status.success(), "gh api graphql failed: {}", String::from_utf8_lossy(&out.stderr).trim());
        let v: Value = serde_json::from_slice(&out.stdout)?;
        review_state_from(v.pointer("/data/repository/pullRequest").context("graphql: no pullRequest")?)
    }

    fn rebase(&self, n: i64) -> Result<()> {
        self.gh(&["pr", "update-branch", &n.to_string(), "--rebase"]).map(drop) // no-op when up to date
    }

    fn block_on(&self, child: i64, parent: i64) -> Result<()> {
        bail!("GitHub has no MR dependencies: can't block #{child} on #{parent} (this forge doesn't stack)")
    }

    fn arm_merge(&self, n: i64) -> Result<()> {
        // Merges at once if the PR is already mergeable; otherwise waits for its required checks.
        self.gh(&["pr", "merge", &n.to_string(), "--auto", "--squash"]).map(drop)
    }

    fn disarm(&self, n: i64) -> Result<()> {
        if self.view(n)?.get("autoMergeRequest").is_none_or(Value::is_null) {
            return Ok(()); // not armed — gh would refuse the disable
        }
        self.gh(&["pr", "merge", &n.to_string(), "--disable-auto"]).map(drop)
    }

    fn set_draft(&self, n: i64, draft: bool) -> Result<()> {
        // `gh pr ready` is idempotent both ways; converting to draft also drops auto-merge.
        let n = n.to_string();
        let args = if draft { vec!["pr", "ready", n.as_str(), "--undo"] } else { vec!["pr", "ready", n.as_str()] };
        self.gh(&args).map(drop)
    }

    fn open_mr(&self, d: &MrDraft) -> Result<String> {
        let create = |draft: bool| {
            let mut args = vec![
                "pr", "create", "--base", &d.target, "--head", &d.source,
                "--title", &d.title, "--body", &d.description, "--assignee", "@me",
            ];
            if draft {
                args.push("--draft");
            }
            self.gh(&args)
        };
        // Squash is chosen at merge (`arm_merge`), not here. Drafts are a plan feature on private repos.
        let out = match create(d.draft) {
            Err(e) if d.draft && e.to_string().to_lowercase().contains("draft") => {
                eprintln!("fleetd: ⚠ {}: drafts unavailable here, opening {} ready instead ({e:#})", self.repo, d.source);
                create(false)?
            }
            other => other?,
        };
        out.lines()
            .rev()
            .find(|l| l.trim_start().starts_with("http"))
            .map(|l| l.trim().to_string())
            .ok_or_else(|| anyhow!("gh pr create printed no URL: {out}"))
    }

    fn request_review(&self, ticket: &TicketKey) -> Result<Vec<String>> {
        let pr = self
            .armada()?
            .into_iter()
            .find(|s| s.ticket.as_ref() == Some(ticket) && matches!(s.state, ShipState::Open | ShipState::Draft))
            .ok_or_else(|| anyhow!("request_review {}: no open PR for the ticket", ticket.0))?;
        self.set_draft(pr.iid, false)?; // ready → GitHub requests the CODEOWNERS itself
        Ok(vec![])
    }
}

/// Map one PR's JSON (`FIELDS`) onto a ship. `open_threads` needs GraphQL, so `snapshot` fills it.
fn pr_to_ship(p: &Value, prefix: &str) -> Option<ForgeShip> {
    let s = |k: &str| p.get(k).and_then(Value::as_str).unwrap_or("");
    let title = s("title").to_string();
    let draft = p.get("isDraft").and_then(Value::as_bool).unwrap_or(false);
    let state = match s("state") {
        "MERGED" => ShipState::Merged,
        "CLOSED" => ShipState::Closed,
        _ if draft => ShipState::Draft,
        _ => ShipState::Open,
    };
    let approved = p
        .get("latestReviews")
        .and_then(Value::as_array)
        .map_or(0, |r| r.iter().filter(|r| r.get("state").and_then(Value::as_str) == Some("APPROVED")).count() as i64);
    let pipeline = pipeline_status(p.get("statusCheckRollup").and_then(Value::as_array).map_or(&[], |v| v.as_slice()));
    Some(ForgeShip {
        iid: p.get("number")?.as_i64()?,
        url: s("url").to_string(),
        target: s("baseRefName").to_string(),
        pilot: p.pointer("/author/login").and_then(Value::as_str).unwrap_or("").to_string(),
        ticket: TicketKey::find_in(s("headRefName"), prefix).or_else(|| TicketKey::find_in(&title, prefix)),
        title,
        state,
        head_sha: s("headRefOid").to_string(),
        has_conflicts: s("mergeable") == "CONFLICTING",
        open_threads: 0,
        approvals: approvals(s("reviewDecision"), approved),
        armed: p.get("autoMergeRequest").is_some_and(|a| !a.is_null()),
        on_merge_train: false,
        merge_status: merge_status(s("mergeStateStatus"), s("reviewDecision"), pipeline),
        pipeline,
    })
}

/// GitHub's review decision → our approvals count. GitHub exposes no required-approval count,
/// so `required` is inferred: met when APPROVED, one more than given when review is still owed.
fn approvals(decision: &str, approved: i64) -> Approvals {
    let required = match decision {
        "REVIEW_REQUIRED" | "CHANGES_REQUESTED" => approved + 1,
        "APPROVED" => approved,
        _ => 0, // no review rule on the branch
    };
    Approvals { current: approved, required }
}

/// The check rollup → one pipeline status: any failure fails it, anything unfinished keeps it
/// running, else it passed. No checks at all → `None`.
fn pipeline_status(checks: &[Value]) -> Option<PipelineStatus> {
    if checks.is_empty() {
        return None;
    }
    // A CheckRun carries status + conclusion; a legacy StatusContext carries `state`.
    let outcome = |c: &Value| {
        let field = |k: &str| c.get(k).and_then(Value::as_str).unwrap_or("").to_ascii_uppercase();
        let (status, conclusion, state) = (field("status"), field("conclusion"), field("state"));
        if matches!(conclusion.as_str(), "FAILURE" | "TIMED_OUT" | "CANCELLED" | "ACTION_REQUIRED" | "STARTUP_FAILURE")
            || matches!(state.as_str(), "FAILURE" | "ERROR")
        {
            Some(PipelineStatus::Failed)
        } else if (!status.is_empty() && status != "COMPLETED") || state == "PENDING" || state == "EXPECTED" {
            Some(PipelineStatus::Running)
        } else {
            None // passed / neutral / skipped
        }
    };
    let outcomes: Vec<_> = checks.iter().map(outcome).collect();
    if outcomes.contains(&Some(PipelineStatus::Failed)) {
        Some(PipelineStatus::Failed)
    } else if outcomes.contains(&Some(PipelineStatus::Running)) {
        Some(PipelineStatus::Running)
    } else {
        Some(PipelineStatus::Success)
    }
}

/// `mergeStateStatus` → GitLab's `merge_status` vocabulary (what the sailor reads). BLOCKED is
/// split by its cause: an owed review, else unfinished or failing checks.
fn merge_status(state: &str, decision: &str, pipeline: Option<PipelineStatus>) -> Option<String> {
    let word = match state {
        "CLEAN" | "HAS_HOOKS" => "mergeable",
        "DIRTY" => "conflict",
        "BEHIND" => "need_rebase",
        "DRAFT" => "draft_status",
        "UNSTABLE" => "ci_must_pass",
        "BLOCKED" if matches!(decision, "REVIEW_REQUIRED" | "CHANGES_REQUESTED") => "not_approved",
        "BLOCKED" if pipeline != Some(PipelineStatus::Success) => "ci_must_pass",
        "BLOCKED" => "blocked_status",
        "UNKNOWN" | "" => "checking",
        _ => return Some(state.to_ascii_lowercase()),
    };
    Some(word.to_string())
}

/// The GraphQL `pullRequest` node → the review surface: unresolved threads gate the merge; PR
/// comments and non-empty review bodies are the floating notes (they gate approval, not merge).
fn review_state_from(pr: &Value) -> Result<ReviewState> {
    let nodes = |k: &str| pr.pointer(&format!("/{k}/nodes")).and_then(Value::as_array).cloned().unwrap_or_default();
    let open_threads = nodes("reviewThreads")
        .iter()
        .filter(|t| !t.get("isResolved").and_then(Value::as_bool).unwrap_or(true))
        .count() as i64;
    let note = |n: &Value| FloatingNote {
        id: n.get("databaseId").and_then(Value::as_i64).unwrap_or(0),
        author: n.pointer("/author/login").and_then(Value::as_str).map(String::from),
        created_at: n.get("createdAt").and_then(Value::as_str).unwrap_or("").to_string(),
        body: n.get("body").and_then(Value::as_str).unwrap_or("").to_string(),
    };
    let mut floating_notes: Vec<FloatingNote> =
        nodes("comments").iter().chain(nodes("reviews").iter()).map(note).filter(|n| !n.body.trim().is_empty()).collect();
    floating_notes.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    Ok(ReviewState { open_threads, floating_notes })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The probe PR's shape, as `gh pr list --json FIELDS` returned it.
    fn pr(draft: bool, merge_state: &str, checks: Value) -> Value {
        json!({
            "number": 2, "url": "https://github.com/me/armada-test/pull/2", "baseRefName": "main",
            "headRefName": "chore/ARMT-2-probe", "author": {"login": "me"}, "title": "ARMT-2: add mul",
            "isDraft": draft, "state": "OPEN", "headRefOid": "45649ff", "mergeable": "MERGEABLE",
            "mergeStateStatus": merge_state, "reviewDecision": "", "statusCheckRollup": checks,
            "autoMergeRequest": null, "latestReviews": []
        })
    }

    #[test]
    fn maps_a_pr_onto_a_ship() {
        let running = json!([{"__typename": "CheckRun", "name": "test", "status": "IN_PROGRESS", "conclusion": ""}]);
        let s = pr_to_ship(&pr(true, "BLOCKED", running), "ARMT").unwrap();
        assert_eq!((s.iid, s.target.as_str(), s.pilot.as_str()), (2, "main", "me"));
        assert_eq!(s.ticket, Some(TicketKey("ARMT-2".into())), "the key from the branch");
        assert_eq!(s.state, ShipState::Draft, "isDraft wins: a draft reports BLOCKED, not DRAFT");
        assert_eq!(s.pipeline, Some(PipelineStatus::Running));
        assert_eq!(s.merge_status.as_deref(), Some("ci_must_pass"));
        assert!(!s.armed && !s.has_conflicts);
        assert_eq!(pr_to_ship(&pr(false, "CLEAN", json!([])), "OTHER").unwrap().ticket, None, "prefix honored");
    }

    #[test]
    fn check_rollup_collapses_to_one_status() {
        let run = |status: &str, conclusion: &str| json!({"status": status, "conclusion": conclusion});
        assert_eq!(pipeline_status(&[]), None);
        assert_eq!(pipeline_status(&[run("COMPLETED", "SUCCESS"), run("COMPLETED", "SKIPPED")]), Some(PipelineStatus::Success));
        assert_eq!(pipeline_status(&[run("COMPLETED", "SUCCESS"), run("QUEUED", "")]), Some(PipelineStatus::Running));
        assert_eq!(pipeline_status(&[run("IN_PROGRESS", ""), run("COMPLETED", "FAILURE")]), Some(PipelineStatus::Failed));
        assert_eq!(pipeline_status(&[json!({"state": "PENDING"})]), Some(PipelineStatus::Running), "a StatusContext");
    }

    #[test]
    fn merge_state_speaks_gitlabs_words() {
        let ok = Some(PipelineStatus::Success);
        assert_eq!(merge_status("CLEAN", "", ok).as_deref(), Some("mergeable"));
        assert_eq!(merge_status("DIRTY", "", ok).as_deref(), Some("conflict"));
        assert_eq!(merge_status("BEHIND", "", ok).as_deref(), Some("need_rebase"));
        assert_eq!(merge_status("BLOCKED", "REVIEW_REQUIRED", ok).as_deref(), Some("not_approved"));
        assert_eq!(merge_status("BLOCKED", "", Some(PipelineStatus::Running)).as_deref(), Some("ci_must_pass"));
        assert_eq!(merge_status("UNKNOWN", "", None).as_deref(), Some("checking"));
    }

    #[test]
    fn approvals_infer_the_requirement_from_the_decision() {
        assert_eq!(approvals("", 0), Approvals { current: 0, required: 0 });
        assert_eq!(approvals("REVIEW_REQUIRED", 0), Approvals { current: 0, required: 1 });
        assert_eq!(approvals("APPROVED", 1), Approvals { current: 1, required: 1 });
    }

    #[test]
    fn review_state_counts_unresolved_threads_and_keeps_bodied_notes() {
        let pr = json!({
            "reviewThreads": {"nodes": [{"isResolved": false}, {"isResolved": true}]},
            "comments": {"nodes": [{"databaseId": 9, "author": {"login": "me"}, "createdAt": "2026-10-25T20:28:37Z", "body": "Probe floating note."}]},
            "reviews": {"nodes": [{"databaseId": 7, "author": {"login": "rev"}, "createdAt": "2026-10-25T20:20:00Z", "body": ""}]}
        });
        let r = review_state_from(&pr).unwrap();
        assert_eq!(r.open_threads, 1);
        assert_eq!(r.floating_notes.len(), 1, "an empty review body isn't a note");
        assert_eq!(r.floating_notes[0].body, "Probe floating note.");
    }

    /// Live: reads the sandbox's open PRs through `gh`. `FLEET_GH_SMOKE=owner/repo cargo test -p fleetd -- --ignored`.
    #[test]
    #[ignore]
    fn smoke_reads_the_live_sandbox() {
        let repo = std::env::var("FLEET_GH_SMOKE").expect("FLEET_GH_SMOKE=owner/repo");
        let f = GhForge::new(repo, "ARMT");
        println!("preflight: {}", f.preflight().unwrap());
        let ships = f.armada().unwrap();
        for s in &ships {
            let snap = f.snapshot(s.iid).unwrap();
            println!("#{} {:?} {:?} pipeline={:?} merge={:?} threads={}", s.iid, s.ticket, snap.state, snap.pipeline, snap.merge_status, snap.open_threads);
            println!("  notes: {:?}", f.review_state(s.iid).unwrap().floating_notes.len());
        }
        assert!(!ships.is_empty(), "the sandbox keeps one open PR as a fixture");
    }

    /// Live: drives the write path on the sandbox's fixture PR (`FLEET_GH_SMOKE_PR`, a green draft)
    /// and opens a draft PR from `FLEET_GH_SMOKE_BRANCH` (pushed beforehand). Never arms — a green
    /// PR would merge at once. `-- --ignored smoke_drives`.
    #[test]
    #[ignore]
    fn smoke_drives_the_write_path() {
        let env = |k: &str| std::env::var(k).unwrap_or_else(|_| panic!("{k} unset"));
        let f = GhForge::new(env("FLEET_GH_SMOKE"), "ARMT");
        let n: i64 = env("FLEET_GH_SMOKE_PR").parse().unwrap();
        let draft = |f: &GhForge| f.snapshot(n).unwrap().state == ShipState::Draft;
        f.set_draft(n, false).unwrap();
        f.set_draft(n, false).unwrap(); // idempotent
        assert!(!draft(&f));
        f.disarm(n).unwrap(); // not armed → a no-op, not an error
        f.set_draft(n, true).unwrap();
        f.set_draft(n, true).unwrap();
        assert!(draft(&f));
        f.rebase(n).unwrap(); // up to date → a no-op
        let ticket = f.snapshot(n).unwrap().ticket.unwrap();
        assert_eq!(f.request_review(&ticket).unwrap(), Vec::<String>::new());
        assert!(!draft(&f), "request_review marks it ready");
        f.set_draft(n, true).unwrap(); // leave the fixture as found
        assert!(f.block_on(n, 1).is_err(), "GitHub can't stack");

        let branch = env("FLEET_GH_SMOKE_BRANCH");
        let d = MrDraft { source: branch.clone(), target: "main".into(), title: format!("{branch}: smoke"), description: "Opened by the smoke test.".into(), draft: true };
        let url = f.open_mr(&d).unwrap();
        println!("opened {url}");
        assert!(url.contains("/pull/"));
    }
}
