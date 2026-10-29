//! The `jira` Tracker driver — shells out to `jira-cli` for reads, creates and transitions.
//! Field edits (description, labels) go to Jira's REST API through `curl` instead, with jira-cli's
//! own credentials: `jira issue edit` re-sends every component on the ticket, so one Jira no longer
//! accepts (an archived component) fails every edit.
//!
//! The vendor→domain status mapping lives here (and ONLY here): Jira's status names
//! map onto `TicketStatus`. `core` never names Jira.

use anyhow::{ensure, Context, Result};
use domain::ports::Tracker;
use domain::{ConvoyKey, TicketDetail, TicketDraft, TicketKey, TicketStatus, TrackerTicket};
use std::process::Command;

/// `[tracker]` in fleet.toml: the Jira `project` (its key is the ticket prefix) and the optional
/// label a worker's claim adds. Project-scoped commands pass `-p <project>`, so fleet.toml — not
/// jira-cli's own default — picks the project. The sprint board still comes from `jira init`.
pub struct JiraTracker {
    project: String,
    claim_label: Option<String>,
}

impl JiraTracker {
    pub fn new(project: impl Into<String>, claim_label: Option<String>) -> Self {
        Self { project: project.into(), claim_label }
    }

    fn raw_page(&self, jql: &str, from: usize) -> Result<String> {
        let out = Command::new("jira")
            .args(["issue", "list", "-p", &self.project, "-q", jql, "--raw", "--paginate", &format!("{from}:100")])
            .output()?;
        ensure!(
            out.status.success(),
            "jira issue list failed: {}",
            crate::preflight::one_line(&out.stderr)
        );
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// Fetch the sprint. jira-cli caps a page at 100; most sprints fit, so we take
    /// one page for now — multi-page pagination is a cheap refinement when needed.
    fn all_issues(&self, jql: &str) -> Result<Vec<TrackerTicket>> {
        Ok(parse_page(&self.raw_page(jql, 0)?)?.1)
    }

    /// Run `jira <args>` and return stdout on success (stderr on failure). The shared
    /// invocation path for the claim mutations.
    fn jira(&self, args: &[&str]) -> Result<String> {
        let out = Command::new("jira").args(args).output()?;
        ensure!(
            out.status.success(),
            "jira {} failed: {}",
            args.join(" "),
            crate::preflight::one_line(&out.stderr)
        );
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// assign to the operator. jira-cli's `assign` needs an exact email (no `me` keyword),
    /// so resolve it via `jira me` first — the documented self-assign idiom.
    fn assign_to_me(&self, key: &TicketKey) -> Result<()> {
        let me = self.jira(&["me"])?;
        let me = me.trim();
        ensure!(!me.is_empty(), "jira me returned empty");
        self.jira(&["issue", "assign", &key.0, me])?;
        Ok(())
    }

    /// One issue by key — `jira issue view --raw`, a direct fetch. Never a JQL search: Jira's search
    /// index lags a create by seconds, so a just-filed ticket isn't found by `key = X` yet.
    fn view(&self, key: &TicketKey) -> Result<serde_json::Value> {
        Ok(serde_json::from_str(&self.jira(&["issue", "view", &key.0, "--raw"])?)?)
    }

    fn add_claim_label(&self, key: &TicketKey) -> Result<()> {
        match &self.claim_label {
            Some(label) => self.label(key, label, true),
            None => Ok(()),
        }
    }

    /// Set fields on an issue — `PUT /rest/api/2/issue/<KEY>` with only the given `body`, so the
    /// ticket's other fields (its components) aren't re-sent. jira-cli's credentials: the server +
    /// login from its config, the token from `JIRA_API_TOKEN` (passed on stdin, never in argv),
    /// else `~/.netrc`.
    fn put_issue(&self, key: &TicketKey, body: &serde_json::Value) -> Result<()> {
        let cfg = jira_cli_config()?;
        let url = format!("{}/rest/api/2/issue/{}", cfg.server.trim_end_matches('/'), key.0);
        let token = std::env::var("JIRA_API_TOKEN").ok().filter(|t| !t.is_empty());
        let mut curl = Command::new("curl");
        curl.args(["-sS", "-X", "PUT", "-H", "Content-Type: application/json", "-w", "\n%{http_code}"])
            .args(["--data-binary", &body.to_string(), &url])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        curl.args(if token.is_some() { ["-K", "-"] } else { ["--netrc", "-f"] });
        let mut child = curl.spawn().context("curl")?;
        if let Some(t) = &token {
            let user = format!("user = \"{}:{}\"\n", cfg.login, t.replace('\\', "\\\\").replace('"', "\\\""));
            std::io::Write::write_all(child.stdin.as_mut().context("curl stdin")?, user.as_bytes())?;
        }
        drop(child.stdin.take());
        let out = child.wait_with_output()?;
        let stdout = String::from_utf8_lossy(&out.stdout);
        let (resp, code) = stdout.rsplit_once('\n').unwrap_or(("", &stdout));
        ensure!(
            out.status.success() && code.starts_with('2'),
            "jira PUT {}: {code} {}{}",
            key.0,
            crate::preflight::one_line(resp.as_bytes()),
            crate::preflight::one_line(&out.stderr)
        );
        Ok(())
    }

    /// add to the active sprint. jira-cli 1.7.0: `sprint list --plain --columns ID` + the
    /// positional `sprint add <SPRINT_ID> <ISSUE>` are the verified forms.
    fn add_to_active_sprint(&self, key: &TicketKey) -> Result<()> {
        let list = self.jira(&[
            "sprint", "list", "-p", &self.project, "--state", "active", "--plain", "--no-headers", "--columns", "ID",
        ])?;
        let id = list
            .lines()
            .next()
            .and_then(|l| l.split_whitespace().next())
            .context("no active sprint")?;
        self.jira(&["sprint", "add", id, &key.0])?;
        Ok(())
    }
}

impl Tracker for JiraTracker {
    fn preflight(&self) -> Result<String> {
        let me = crate::preflight::cli("jira", &["me"])?; // installed + authenticated (`jira init`)
        let projects = crate::preflight::cli("jira", &["project", "list"])?;
        ensure!(
            projects.split_whitespace().any(|w| w == self.project),
            "project {} not visible to {me} (jira project list)",
            self.project
        );
        Ok(format!("jira logged in as {me} · project {}", self.project))
    }

    fn board(&self) -> Result<Vec<TrackerTicket>> {
        self.all_issues("sprint in openSprints()") // `-p` scopes it to the project
    }

    fn get(&self, key: &TicketKey) -> Result<TrackerTicket> {
        issue_to_ticket(&self.view(key)?).with_context(|| format!("jira issue view {}: unexpected shape", key.0))
    }

    fn comment(&self, key: &TicketKey, body: &str) -> Result<()> {
        self.jira(&["issue", "comment", "add", &key.0, body, "--no-input"]).map(drop)
    }

    fn transition(&self, key: &TicketKey, to: TicketStatus) -> Result<()> {
        // Idempotent: a Jira workflow has no self-transition, so `jira issue move` to the status a
        // ticket already holds fails — yet "already there" is success, not error. This matters for
        // a HAND dispatched onto an already-in-flight ticket (its status is past To Do), where the
        // claim's transition would otherwise abort the spawn.
        if self.get(key)?.status == to {
            return Ok(());
        }
        let out = Command::new("jira")
            .args(["issue", "move", &key.0, jira_status_name(to)])
            .output()?;
        ensure!(out.status.success(), "jira move failed: {}", crate::preflight::one_line(&out.stderr));
        Ok(())
    }

    /// The claim gate (claim.sh step 0): JQL asks Jira itself whether a PEER owns it —
    /// `currentUser()` resolves the operator, so no accountId plumbing. A matching row = a peer.
    /// jira-cli exits NON-ZERO when the query matches nothing — and nothing-matches is exactly the
    /// claimable case (no peer owns it), NOT an error. So we can't route this through `self.jira()`
    /// (which `ensure!`s a zero exit); we read the exit + the documented "No result found" message.
    fn assigned_to_peer(&self, key: &TicketKey) -> Result<bool> {
        let jql = format!("key = {} AND assignee IS NOT EMPTY AND assignee != currentUser()", key.0);
        let out = Command::new("jira")
            .args(["issue", "list", "-p", &self.project, "-q", &jql, "--plain", "--no-headers", "--columns", "KEY"])
            .output()?;
        if out.status.success() {
            return Ok(!String::from_utf8_lossy(&out.stdout).trim().is_empty());
        }
        let err = crate::preflight::one_line(&out.stderr);
        ensure!(err.contains("No result found"), "jira assignee check failed: {err}");
        Ok(false) // empty match → no peer → claimable
    }

    fn claim(&self, key: &TicketKey) -> Result<()> {
        let failed: Vec<String> = [("assign", self.assign_to_me(key)), ("label", self.add_claim_label(key)), ("sprint", self.add_to_active_sprint(key))]
            .into_iter()
            .filter_map(|(step, res)| res.err().map(|e| format!("{step}: {e:#}")))
            .collect();
        ensure!(failed.is_empty(), "partial claim — {}", failed.join("; "));
        Ok(())
    }

    fn parent_of(&self, key: &TicketKey) -> Result<Option<TicketKey>> {
        // The issue's `fields.parent.key` (absent for non-sub-tasks).
        let parent = self.view(key)?.pointer("/fields/parent/key").and_then(|k| k.as_str()).map(String::from);
        Ok(parent.and_then(|k| TicketKey::parse(&k).ok()))
    }

    fn children(&self, parent: &TicketKey) -> Result<Vec<TrackerTicket>> {
        self.all_issues(&format!("parent = {}", parent.0))
    }

    fn read(&self, key: &TicketKey) -> Result<TicketDetail> {
        detail_from_issue(&self.view(key)?).with_context(|| format!("jira issue view {}: unexpected shape", key.0))
    }

    fn create(&self, d: &TicketDraft) -> Result<TicketKey> {
        let kind = capitalized(&d.kind); // jira's type names: Story, Bug, Task, Epic
        let mut args = vec!["issue", "create", "-p", &self.project, "-t", &kind, "-s", &d.title, "-b", &d.description];
        if let Some(convoy) = &d.convoy {
            args.extend(["-P", &convoy.0]);
        }
        for label in &d.labels {
            args.extend(["-l", label]);
        }
        args.extend(["--no-input", "--raw"]);
        let out = self.jira(&args)?;
        let key = serde_json::from_str::<serde_json::Value>(&out)
            .ok()
            .and_then(|v| v.get("key").and_then(|k| k.as_str()).and_then(|k| TicketKey::parse(k).ok()))
            .or_else(|| TicketKey::find_in(&out, &self.project))
            .with_context(|| format!("jira issue create printed no {}-N key: {out}", self.project))?;
        for blocker in &d.blocked_by {
            self.block(&key, blocker)?;
        }
        Ok(key)
    }

    fn edit(&self, key: &TicketKey, description: &str) -> Result<()> {
        self.put_issue(key, &serde_json::json!({ "fields": { "description": wiki(description) } }))
    }

    fn label(&self, key: &TicketKey, label: &str, on: bool) -> Result<()> {
        let op = if on { "add" } else { "remove" };
        self.put_issue(key, &serde_json::json!({ "update": { "labels": [{ op: label }] } }))
    }

    fn block(&self, key: &TicketKey, by: &TicketKey) -> Result<()> {
        // `jira issue link INWARD OUTWARD Blocks` reads "INWARD blocks OUTWARD" (Jira's link
        // convention), so "key is blocked by `by`" is `link by key`. UNVERIFIED against a live Jira.
        if self.get(key)?.blocked_by.contains(by) {
            return Ok(()); // idempotent: the edge exists
        }
        self.jira(&["issue", "link", &by.0, &key.0, "Blocks"]).map(drop)
    }
}

/// jira-cli's config (`jira init`): where Jira is and who logs in. `JIRA_CONFIG_FILE` overrides
/// the default `~/.config/.jira/.config.yml`, as it does for jira-cli.
#[derive(serde::Deserialize)]
struct JiraCliConfig {
    server: String,
    login: String,
}

fn jira_cli_config() -> Result<JiraCliConfig> {
    let path = match std::env::var("JIRA_CONFIG_FILE") {
        Ok(p) if !p.is_empty() => p,
        _ => format!("{}/.config/.jira/.config.yml", std::env::var("HOME").context("HOME")?),
    };
    let yaml = std::fs::read_to_string(&path).with_context(|| format!("reading jira-cli config {path}"))?;
    serde_saphyr::from_str(&yaml).map_err(|e| anyhow::anyhow!("jira-cli config {path}: {e}"))
}

/// Markdown → Jira wiki markup (what REST v2 takes for a description; Jira stores it as ADF): the
/// inverse of `adf_text` — headings, lists, code blocks, quotes, and inline code / bold / italic /
/// links. jira-cli did this conversion for us; a REST edit does it here.
fn wiki(md: &str) -> String {
    let mut out = Vec::new();
    let mut fence = false;
    for line in md.lines() {
        if let Some(lang) = line.trim_start().strip_prefix("```") {
            out.push(match (fence, lang.trim()) {
                (true, _) => "{code}".to_string(),
                (false, "") => "{code}".to_string(),
                (false, l) => format!("{{code:{l}}}"),
            });
            fence = !fence;
            continue;
        }
        if fence {
            out.push(line.to_string());
            continue;
        }
        let indent = (line.len() - line.trim_start().len()) / 2;
        let t = line.trim_start();
        let hashes = t.chars().take_while(|&c| c == '#').count();
        let digits = t.chars().take_while(char::is_ascii_digit).count();
        out.push(if (1..=6).contains(&hashes) && t[hashes..].starts_with(' ') {
            format!("h{hashes}. {}", inline(t[hashes..].trim_start()))
        } else if let Some(q) = t.strip_prefix("> ") {
            format!("bq. {}", inline(q))
        } else if let Some(item) = t.strip_prefix("- ").or_else(|| t.strip_prefix("* ")) {
            format!("{} {}", "*".repeat(indent + 1), inline(item))
        } else if digits > 0 && t[digits..].starts_with(". ") {
            format!("{} {}", "#".repeat(indent + 1), inline(&t[digits + 2..]))
        } else {
            inline(line)
        });
    }
    out.join("\n")
}

/// One line's inline markdown → wiki: `` `x` `` → `{{x}}`, `**x**` → `*x*`, `*x*` → `_x_`,
/// `[t](u)` → `[t|u]`. An unclosed marker stays as written.
fn inline(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(c) = rest.chars().next() {
        let closed = |open: &str, close: &str| rest.strip_prefix(open).and_then(|r| r.find(close).map(|i| (&r[..i], &r[i + close.len()..])));
        let (piece, tail) = if let Some((code, tail)) = closed("`", "`") {
            (format!("{{{{{code}}}}}"), tail)
        } else if let Some((bold, tail)) = closed("**", "**") {
            (format!("*{}*", inline(bold)), tail)
        } else if let Some((em, tail)) = closed("*", "*").filter(|(em, _)| !em.is_empty() && !em.starts_with(' ')) {
            (format!("_{}_", inline(em)), tail)
        } else if let Some((text, url, tail)) =
            closed("[", "](").and_then(|(text, r)| r.find(')').map(|i| (text, &r[..i], &r[i + 1..])))
        {
            (format!("[{}|{url}]", inline(text)), tail)
        } else {
            (c.to_string(), &rest[c.len_utf8()..])
        };
        out.push_str(&piece);
        rest = tail;
    }
    out
}

/// "story" → "Story".
fn capitalized(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
}

/// One `jira issue view --raw` issue → its full text: the board facts, the issue type, and the
/// description (Jira Cloud's ADF document flattened to text; a plain string as is).
fn detail_from_issue(issue: &serde_json::Value) -> Option<TicketDetail> {
    let f = issue.get("fields")?;
    Some(TicketDetail {
        ticket: issue_to_ticket(issue)?,
        kind: f.pointer("/issuetype/name").and_then(|n| n.as_str()).unwrap_or("").to_lowercase(),
        description: match f.get("description") {
            Some(serde_json::Value::String(s)) => s.clone(),
            Some(doc) if doc.is_object() => adf_text(doc).trim_end().to_string(),
            _ => String::new(),
        },
        held: None,
        location: browse_url(issue).unwrap_or_default(),
        labels: f
            .get("labels")
            .and_then(|l| l.as_array())
            .map(|l| l.iter().filter_map(|x| x.as_str().map(String::from)).collect())
            .unwrap_or_default(),
    })
}

/// The issue's browse page, from its REST `self` link: `https://<host>/rest/api/…` →
/// `https://<host>/browse/<KEY>`.
fn browse_url(issue: &serde_json::Value) -> Option<String> {
    let api = issue.get("self")?.as_str()?;
    let host = api.split("/rest/").next()?;
    Some(format!("{host}/browse/{}", issue.get("key")?.as_str()?))
}

/// Render an Atlassian Document Format node as markdown — the blocks (headings, paragraphs, lists,
/// code blocks, quotes) and the inline marks (code, bold, italic, links) — so what `read` returns,
/// `edit` takes back without losing the formatting.
fn adf_text(node: &serde_json::Value) -> String {
    let str_at = |ptr: &str| node.pointer(ptr).and_then(|v| v.as_str()).unwrap_or("");
    let children = || node.get("content").and_then(|c| c.as_array()).into_iter().flatten();
    let inner = || children().map(adf_text).collect::<String>();
    match str_at("/type") {
        "text" => marked(str_at("/text"), node),
        "hardBreak" => "\n".into(),
        // A smart link: an issue's (`…/browse/KEY…`) reads as its bare key, which Jira links again
        // on write; any other URL as is.
        "inlineCard" => {
            let url = str_at("/attrs/url");
            url.split_once("/browse/").map_or(url, |(_, k)| k.split(['#', '?', '/']).next().unwrap_or(k)).into()
        }
        "heading" => {
            let level = node.pointer("/attrs/level").and_then(|l| l.as_u64()).unwrap_or(2) as usize;
            format!("{} {}\n\n", "#".repeat(level), inner())
        }
        "paragraph" => format!("{}\n\n", inner()),
        "codeBlock" => format!("```{}\n{}\n```\n\n", str_at("/attrs/language"), inner()),
        "blockquote" => format!("{}\n", inner().trim_end().lines().map(|l| format!("> {l}\n")).collect::<String>()),
        "listItem" => format!("- {}\n", inner().trim_end()),
        "bulletList" => format!("{}\n", inner()),
        "orderedList" => {
            let items = children().map(adf_text).enumerate();
            let items = items.map(|(i, item)| format!("{}. {}", i + 1, item.strip_prefix("- ").unwrap_or(&item)));
            format!("{}\n", items.collect::<String>())
        }
        _ => inner(),
    }
}

/// A text node's markdown: its text wrapped in its marks, code innermost.
fn marked(text: &str, node: &serde_json::Value) -> String {
    let marks: Vec<&serde_json::Value> = node.get("marks").and_then(|m| m.as_array()).into_iter().flatten().collect();
    let mark = |kind: &str| marks.iter().find(|m| m.get("type").and_then(|t| t.as_str()) == Some(kind));
    let mut t = text.to_string();
    for (kind, wrap) in [("code", "`"), ("em", "*"), ("strong", "**")] {
        if mark(kind).is_some() {
            t = format!("{wrap}{t}{wrap}");
        }
    }
    match mark("link").and_then(|m| m.pointer("/attrs/href")).and_then(|h| h.as_str()) {
        Some(href) => format!("[{t}]({href})"),
        None => t,
    }
}

/// Parse one page → (raw issue count, mapped tickets). The raw count drives
/// pagination; unknown-status issues are dropped from the mapped set.
fn parse_page(json: &str) -> Result<(usize, Vec<TrackerTicket>)> {
    let v: serde_json::Value = serde_json::from_str(json)?;
    let issues = v
        .get("issues")
        .and_then(|i| i.as_array())
        .or_else(|| v.as_array())
        .context("expected {issues:[...]} (or an array) from jira")?;
    Ok((issues.len(), issues.iter().filter_map(issue_to_ticket).collect()))
}

fn issue_to_ticket(i: &serde_json::Value) -> Option<TrackerTicket> {
    let key = TicketKey::parse(i.get("key")?.as_str()?).ok()?;
    let f = i.get("fields")?;
    let status_name = f
        .get("status")
        .and_then(|s| s.get("name"))
        .and_then(|n| n.as_str())
        .unwrap_or("");
    Some(TrackerTicket {
        key,
        title: f.get("summary").and_then(|s| s.as_str()).unwrap_or("").to_string(),
        status: jira_status(status_name)?, // unknown status ⇒ skip (filter_map)
        assignee: f
            .get("assignee")
            .and_then(|a| a.get("displayName"))
            .and_then(|n| n.as_str())
            .map(String::from),
        // The convoy is the ticket's epic. Team-managed Jira projects link Story→Epic via
        // `parent` (the legacy Epic Link custom field, where present, is a read-only mirror),
        // so `parent` IS the epic/convoy. (A sub-task's parent is its story, not the epic — but
        // sub-tasks aren't on the captain's dispatch frontier, so that's fine.)
        convoy: f
            .get("parent")
            .and_then(|p| p.get("key"))
            .and_then(|k| k.as_str())
            .and_then(|s| ConvoyKey::parse(s).ok()),
        blocked_by: parse_blocked_by(f),
    })
}

/// A ticket's blockers from its `issueLinks`: the inward side of a "Blocks" link — "X is blocked by
/// Y" ⇒ a `Blocks`-type link on X carrying `inwardIssue` = Y. The outward "blocks" side is the
/// reverse edge (carried on the blocker, not here); `Relates` / "Work item split" / etc. are ignored.
/// NB: jira-cli spells the field two ways — `issue list --raw` says `issueLinks`, `issue view
/// --raw` (Jira's own REST shape) says `issuelinks`.
fn parse_blocked_by(fields: &serde_json::Value) -> Vec<TicketKey> {
    let links = fields.get("issueLinks").or_else(|| fields.get("issuelinks"));
    let Some(links) = links.and_then(|l| l.as_array()) else {
        return Vec::new();
    };
    links
        .iter()
        .filter(|l| l.get("type").and_then(|t| t.get("name")).and_then(|n| n.as_str()) == Some("Blocks"))
        .filter_map(|l| l.get("inwardIssue").and_then(|i| i.get("key")).and_then(|k| k.as_str()))
        .filter_map(|k| TicketKey::parse(k).ok())
        .collect()
}

/// Jira status name → our `TicketStatus`. The board column labelled "In Review" is
/// the `In QA` status underneath, so both map to `InQa`.
fn jira_status(name: &str) -> Option<TicketStatus> {
    match name {
        "To Do" => Some(TicketStatus::Todo),
        "Blocked" => Some(TicketStatus::Blocked),
        "In Progress" => Some(TicketStatus::InProgress),
        "In QA" | "In Review" => Some(TicketStatus::InQa),
        "Done" => Some(TicketStatus::Done),
        "Cancelled" | "Canceled" => Some(TicketStatus::Cancelled),
        _ => None,
    }
}

fn jira_status_name(s: TicketStatus) -> &'static str {
    match s {
        TicketStatus::Todo => "To Do",
        TicketStatus::Blocked => "Blocked",
        TicketStatus::InProgress => "In Progress",
        TicketStatus::InQa => "In QA",
        TicketStatus::Done => "Done",
        TicketStatus::Cancelled => "Cancelled",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_an_issue_with_an_adf_description() {
        let issue = serde_json::json!({
            "key": "PROJ-7",
            "self": "https://example.atlassian.net/rest/api/3/issue/10042",
            "fields": {
                "summary": "Add neg", "status": {"name": "To Do"}, "issuetype": {"name": "Story"},
                "labels": ["fleet-proposed"],
                "issuelinks": [{"type": {"name": "Blocks"}, "inwardIssue": {"key": "PROJ-3"}}],
                "description": {"type": "doc", "content": [
                    {"type": "heading", "attrs": {"level": 2}, "content": [{"type": "text", "text": "Outcome"}]},
                    {"type": "paragraph", "content": [
                        {"type": "text", "text": "neg", "marks": [{"type": "code"}, {"type": "strong"}]},
                        {"type": "text", "text": " works ("},
                        {"type": "inlineCard", "attrs": {"url": "https://x.atlassian.net/browse/PROJ-2#icft=PROJ-2"}},
                        {"type": "text", "text": "), per "},
                        {"type": "text", "text": "the spec", "marks": [{"type": "link", "attrs": {"href": "https://x.io/s"}}]},
                        {"type": "text", "text": "."}
                    ]},
                    {"type": "bulletList", "content": [
                        {"type": "listItem", "content": [{"type": "paragraph", "content": [{"type": "text", "text": "AC one"}]}]}
                    ]},
                    {"type": "orderedList", "content": [
                        {"type": "listItem", "content": [{"type": "paragraph", "content": [{"type": "text", "text": "first"}]}]},
                        {"type": "listItem", "content": [{"type": "paragraph", "content": [{"type": "text", "text": "second"}]}]}
                    ]},
                    {"type": "codeBlock", "attrs": {"language": "rust"}, "content": [{"type": "text", "text": "let x = 1;"}]},
                    {"type": "blockquote", "content": [{"type": "paragraph", "content": [{"type": "text", "text": "quoted"}]}]}
                ]}
            }
        });
        let d = detail_from_issue(&issue).unwrap();
        assert_eq!((d.ticket.key.0.as_str(), d.kind.as_str()), ("PROJ-7", "story"));
        assert_eq!(
            d.description,
            "## Outcome\n\n**`neg`** works (PROJ-2), per [the spec](https://x.io/s).\n\n- AC one\n\n1. first\n2. second\n\n```rust\nlet x = 1;\n```\n\n> quoted"
        );
        assert_eq!(capitalized("story"), "Story");
        assert_eq!(d.location, "https://example.atlassian.net/browse/PROJ-7");
        assert_eq!(d.labels, vec!["fleet-proposed".to_string()]);
        assert_eq!(d.ticket.blocked_by, vec![TicketKey("PROJ-3".into())], "view --raw spells it `issuelinks`");
    }

    #[test]
    fn markdown_becomes_jira_wiki_markup() {
        let md = "## Outcome\n\n**`neg`** works, per [the spec](https://x.io/s), *now*.\n\n- AC one\n  - nested\n\n1. first\n2. second\n\n```rust\nlet x = *y*;\n```\n\n> quoted\na * b stays";
        assert_eq!(
            wiki(md),
            "h2. Outcome\n\n*{{neg}}* works, per [the spec|https://x.io/s], _now_.\n\n* AC one\n** nested\n\n# first\n# second\n\n{code:rust}\nlet x = *y*;\n{code}\n\nbq. quoted\na * b stays"
        );
    }

    // Real jira --raw / REST field shape (trimmed).
    const SAMPLE: &str = r#"{"issues":[
      {"key":"PROJ-6192","fields":{"summary":"Capture body","status":{"name":"In Progress"},"assignee":{"displayName":"Jack Shannon"}}},
      {"key":"PROJ-6168","fields":{"summary":"Relocate","status":{"name":"To Do"},"assignee":null}},
      {"key":"PROJ-5000","fields":{"summary":"weird","status":{"name":"Backlog"},"assignee":null}}
    ]}"#;

    #[test]
    fn parses_issues_and_maps_status() {
        let ts = parse_page(SAMPLE).unwrap().1;
        assert_eq!(ts.len(), 2); // "Backlog" is unknown → dropped
        assert_eq!(ts[0].key, TicketKey("PROJ-6192".into()));
        assert_eq!(ts[0].status, TicketStatus::InProgress);
        assert_eq!(ts[0].assignee.as_deref(), Some("Jack Shannon"));
        assert_eq!(ts[1].status, TicketStatus::Todo);
        assert_eq!(ts[1].assignee, None);
    }

    // The live --raw shape (probed): `parent` = the epic; `issueLinks` (camelCase) carries the
    // Blocks edges — inwardIssue = a blocker of this ticket, outwardIssue = one this ticket blocks.
    const LINKED: &str = r#"{"issues":[
      {"key":"PROJ-4160","fields":{"summary":"the blocked one","status":{"name":"To Do"},"parent":{"key":"PROJ-280"},
        "issueLinks":[{"type":{"name":"Blocks","inward":"is blocked by","outward":"blocks"},"inwardIssue":{"key":"PROJ-6412"}}]}},
      {"key":"PROJ-6412","fields":{"summary":"the blocker","status":{"name":"In Progress"},"parent":{"key":"PROJ-280"},
        "issueLinks":[
          {"type":{"name":"Blocks","inward":"is blocked by","outward":"blocks"},"outwardIssue":{"key":"PROJ-4160"}},
          {"type":{"name":"Relates","inward":"relates to","outward":"relates to"},"inwardIssue":{"key":"PROJ-9999"}}]}}
    ]}"#;

    #[test]
    fn maps_convoy_from_parent_and_blocked_by_from_inward_blocks_links() {
        let ts = parse_page(LINKED).unwrap().1;
        let blocked = ts.iter().find(|t| t.key == TicketKey("PROJ-4160".into())).unwrap();
        assert_eq!(blocked.convoy, Some(ConvoyKey("PROJ-280".into())), "convoy = the epic (parent)");
        assert_eq!(blocked.blocked_by, vec![TicketKey("PROJ-6412".into())], "inward Blocks link = a blocker");
        let blocker = ts.iter().find(|t| t.key == TicketKey("PROJ-6412".into())).unwrap();
        assert_eq!(blocker.convoy, Some(ConvoyKey("PROJ-280".into())));
        assert!(
            blocker.blocked_by.is_empty(),
            "the outward 'blocks' edge is the reverse (carried on the blocked ticket); Relates is ignored"
        );
    }

    #[test]
    fn status_mapping_round_trips_and_handles_in_review() {
        assert_eq!(jira_status(jira_status_name(TicketStatus::InQa)), Some(TicketStatus::InQa));
        assert_eq!(jira_status("In Review"), Some(TicketStatus::InQa)); // board label
        assert_eq!(jira_status("Nope"), None);
    }
}
