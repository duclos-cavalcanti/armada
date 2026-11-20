//! The `jira` Tracker driver — shells out to `jira-cli` for reads, creates and transitions.
//! Field edits (description, labels) and unlinks go to Jira's REST API through `curl` instead, with jira-cli's
//! own credentials: `jira issue edit` re-sends every component on the ticket, so one Jira no longer
//! accepts (an archived component) fails every edit.
//!
//! The vendor→domain status mapping lives here (and ONLY here): Jira's status names
//! map onto `TicketStatus`. `core` never names Jira.

use crate::proc::OutputWithin;
use anyhow::{ensure, Context, Result};
use domain::ports::Tracker;
use domain::{ConvoyKey, TicketDetail, TicketDraft, TicketFields, TicketKey, TicketLink, TicketStatus, TrackerTicket};
use std::process::Command;

/// `[tracker]` in fleet.toml: the Jira `project` (its key is the ticket prefix), the optional
/// label a worker's claim adds, and the story-points field's id (it differs per Jira instance).
/// Project-scoped commands pass `-p <project>`, so fleet.toml — not jira-cli's own default — picks
/// the project. The sprint board still comes from `jira init`.
pub struct JiraTracker {
    project: String,
    claim_label: Option<String>,
    points_field: Option<String>,
}

impl JiraTracker {
    pub fn new(project: impl Into<String>, claim_label: Option<String>, points_field: Option<String>) -> Self {
        Self { project: project.into(), claim_label, points_field }
    }

    fn detail(&self, issue: &serde_json::Value) -> Option<TicketDetail> {
        detail_from_issue(issue, self.points_field.as_deref())
    }

    /// The project's `what` (`components`, `versions`) by name → their ids: Jira refuses a
    /// component set by name ("not valid") that it accepts by id. An archived one isn't offered.
    fn ids_by_name(&self, what: &str, names: &[String]) -> Result<Vec<serde_json::Value>> {
        let all = self.rest_json("GET", &format!("/rest/api/2/project/{}/{what}", self.project), None)?;
        let live: Vec<&serde_json::Value> = all.as_array().with_context(|| format!("jira {what}: not a list"))?.iter().filter(|c| c["archived"].as_bool() != Some(true)).collect();
        names
            .iter()
            .map(|name| {
                let id = live.iter().find(|c| c["name"].as_str() == Some(name.as_str())).and_then(|c| c["id"].as_str());
                id.map(|id| serde_json::json!({ "id": id })).with_context(|| {
                    let known: Vec<&str> = live.iter().filter_map(|c| c["name"].as_str()).collect();
                    format!("no {what} entry {name:?} in {} (it has: {})", self.project, known.join(", "))
                })
            })
            .collect()
    }

    /// The first `PAGE` issues the JQL matches, as jira-cli prints them. Only the first: jira-cli
    /// ignores `--paginate`'s offset and returns this page whatever it is told.
    fn first_page(&self, jql: &str) -> Result<String> {
        let out = Command::new("jira")
            .args(["issue", "list", "-p", &self.project, "-q", jql, "--raw", "--paginate", &format!("0:{PAGE}")])
            .output_within(crate::proc::API)?;
        if !out.status.success() {
            // jira-cli fails an empty match ("No result found"): an empty page, not an error.
            let err = crate::preflight::one_line(&out.stderr);
            ensure!(err.contains("No result found"), "jira issue list failed: {err}");
            return Ok("[]".into());
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// Fetch the sprint: its first `PAGE` tickets, all jira-cli returns (today's sprints fit).
    fn all_issues(&self, jql: &str) -> Result<Vec<TrackerTicket>> {
        parse_page(&self.first_page(jql)?)
    }

    /// Run `jira <args>` and return stdout on success (stderr on failure). The shared
    /// invocation path for the claim mutations.
    fn jira(&self, args: &[&str]) -> Result<String> {
        let out = Command::new("jira").args(args).output_within(crate::proc::API)?;
        ensure!(
            out.status.success(),
            "jira {} failed: {}",
            args.join(" "),
            crate::preflight::one_line(&out.stderr)
        );
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
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
    /// ticket's other fields (its components) aren't re-sent.
    fn put_issue(&self, key: &TicketKey, body: &serde_json::Value) -> Result<()> {
        self.rest("PUT", &format!("/rest/api/2/issue/{}", key.0), Some(body))
    }

    /// One call to Jira's REST API (`path` from the server root: `/rest/api/2/…`, `/rest/agile/1.0/…`),
    /// with jira-cli's credentials: the server + login from its config, the token from
    /// `JIRA_API_TOKEN` (passed on stdin, never in argv), else `~/.netrc`.
    fn rest(&self, method: &str, path: &str, body: Option<&serde_json::Value>) -> Result<()> {
        self.rest_json(method, path, body).map(drop)
    }

    /// `rest`, returning the response's JSON (`null` for an empty body).
    fn rest_json(&self, method: &str, path: &str, body: Option<&serde_json::Value>) -> Result<serde_json::Value> {
        let cfg = jira_cli_config()?;
        let url = format!("{}{path}", cfg.server.trim_end_matches('/'));
        let token = std::env::var("JIRA_API_TOKEN").ok().filter(|t| !t.is_empty());
        let mut curl = Command::new("curl");
        curl.args(["-sS", "-X", method, "-H", "Content-Type: application/json", "-w", "\n%{http_code}"]);
        if let Some(body) = body {
            curl.args(["--data-binary", &body.to_string()]);
        }
        curl.arg(&url);
        curl.args(if token.is_some() { ["-K", "-"] } else { ["--netrc", "-f"] });
        let user = token.map(|t| format!("user = \"{}:{}\"\n", cfg.login, t.replace('\\', "\\\\").replace('"', "\\\"")));
        let out = crate::proc::run(&mut curl, user.as_deref().map(str::as_bytes), crate::proc::API).context("curl")?;
        let stdout = String::from_utf8_lossy(&out.stdout);
        let (resp, code) = stdout.rsplit_once('\n').unwrap_or(("", &stdout));
        ensure!(
            out.status.success() && code.starts_with('2'),
            "jira {method} {path}: {code} {}{}",
            crate::preflight::one_line(resp.as_bytes()),
            crate::preflight::one_line(&out.stderr)
        );
        if resp.trim().is_empty() {
            return Ok(serde_json::Value::Null);
        }
        serde_json::from_str(resp).with_context(|| format!("jira {method} {path}: not JSON"))
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
            .output_within(crate::proc::API)?;
        ensure!(out.status.success(), "jira move failed: {}", crate::preflight::one_line(&out.stderr));
        Ok(())
    }

    /// The ownership gate (claim.sh step 0): JQL asks Jira itself which a PEER owns —
    /// `currentUser()` resolves the operator, so no accountId plumbing. One query per hundred keys.
    fn assigned_peers(&self, keys: &[TicketKey]) -> Result<Vec<(TicketKey, String)>> {
        let mut peers = Vec::new();
        for chunk in keys.chunks(100) {
            let keys = chunk.iter().map(|k| k.0.as_str()).collect::<Vec<_>>().join(", ");
            let jql = format!("key in ({keys}) AND assignee IS NOT EMPTY AND assignee != currentUser()");
            peers.extend(self.all_issues(&jql)?.into_iter().map(|t| (t.key, t.assignee.unwrap_or_else(|| "a teammate".into()))));
        }
        Ok(peers)
    }

    /// jira-cli's `assign` needs an exact email (no `me` keyword), so resolve it via `jira me`
    /// first — the documented self-assign idiom.
    fn assign_to_me(&self, key: &TicketKey) -> Result<()> {
        let me = self.jira(&["me"])?;
        let me = me.trim();
        ensure!(!me.is_empty(), "jira me returned empty");
        self.jira(&["issue", "assign", &key.0, me])?;
        Ok(())
    }

    fn claim(&self, key: &TicketKey) -> Result<()> {
        let failed: Vec<String> = [("assign", self.assign_to_me(key)), ("label", self.add_claim_label(key)), ("sprint", self.add_to_active_sprint(key))]
            .into_iter()
            .filter_map(|(step, res)| res.err().map(|e| format!("{step}: {e:#}")))
            .collect();
        ensure!(failed.is_empty(), "partial claim — {}", failed.join("; "));
        Ok(())
    }

    /// UNVERIFIED against a live Jira: the To Do move (the workflow must allow it from where the
    /// ticket stands) and the Agile backlog call.
    fn unclaim(&self, key: &TicketKey) -> Result<()> {
        let failed: Vec<String> = [
            ("todo", self.transition(key, TicketStatus::Todo)),
            ("unassign", self.put_issue(key, &serde_json::json!({ "fields": { "assignee": null } }))),
            ("label", self.claim_label.as_ref().map_or(Ok(()), |l| self.label(key, l, false))),
            ("backlog", self.sprint(key, false)),
        ]
        .into_iter()
        .filter_map(|(step, res)| res.err().map(|e| format!("{step}: {e:#}")))
        .collect();
        ensure!(failed.is_empty(), "partial unclaim — {}", failed.join("; "));
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
        self.detail(&self.view(key)?).with_context(|| format!("jira issue view {}: unexpected shape", key.0))
    }

    /// One JQL search (`key in (…)`) per hundred keys. The search index lags a create by seconds,
    /// so a key it doesn't return yet is read directly.
    fn read_many(&self, keys: &[TicketKey]) -> Result<Vec<TicketDetail>> {
        let mut found = Vec::new();
        for chunk in keys.chunks(100) {
            let jql = format!("key in ({})", chunk.iter().map(|k| k.0.as_str()).collect::<Vec<_>>().join(", "));
            found.extend(page_issues(&self.first_page(&jql)?)?.iter().filter_map(|i| self.detail(i)));
        }
        for key in keys {
            if !found.iter().any(|d: &TicketDetail| &d.ticket.key == key) {
                found.extend(self.read(key).ok());
            }
        }
        Ok(found)
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
            self.link(&key, blocker, TicketLink::BlockedBy, true)?;
        }
        // jira-cli sets components by name, which Jira refuses: they (and the rest) go over REST.
        let set = |v: &[String]| Some(v.to_vec()).filter(|v| !v.is_empty());
        let fields = TicketFields {
            components: set(&d.components),
            fix_versions: set(&d.fix_versions),
            points: d.points,
            priority: d.priority.clone(),
            ..Default::default()
        };
        if !fields.is_empty() {
            self.set_fields(&key, &fields).with_context(|| format!("{} filed, but its fields weren't set", key.0))?;
        }
        Ok(key)
    }

    fn edit(&self, key: &TicketKey, description: &str) -> Result<()> {
        self.put_issue(key, &serde_json::json!({ "fields": { "description": wiki(description) } }))
    }

    /// One `PUT` with only the fields given, so the ticket's others aren't re-sent.
    fn set_fields(&self, key: &TicketKey, f: &TicketFields) -> Result<()> {
        let mut fields = serde_json::Map::new();
        if let Some(title) = &f.title {
            fields.insert("summary".into(), title.as_str().into());
        }
        if let Some(names) = &f.components {
            fields.insert("components".into(), self.ids_by_name("components", names)?.into());
        }
        if let Some(names) = &f.fix_versions {
            fields.insert("fixVersions".into(), self.ids_by_name("versions", names)?.into());
        }
        if let Some(priority) = &f.priority {
            fields.insert("priority".into(), serde_json::json!({ "name": priority }));
        }
        if let Some(points) = f.points {
            let id = self.points_field.as_deref().context("story points need [tracker] points_field in fleet.toml (Jira's field id, e.g. customfield_10004)")?;
            fields.insert(id.into(), points.into());
        }
        let labels: Vec<serde_json::Value> = f.labels.iter().map(|l| serde_json::json!({ "add": l })).collect();
        self.put_issue(key, &serde_json::json!({ "fields": fields, "update": { "labels": labels } }))
    }

    fn search(&self, jql: &str) -> Result<Vec<TicketDetail>> {
        Ok(whole_page(&self.first_page(jql)?, jql)?.iter().filter_map(|i| self.detail(i)).collect())
    }

    fn sprint(&self, key: &TicketKey, active: bool) -> Result<()> {
        match active {
            true => self.add_to_active_sprint(key),
            false => self.rest("POST", "/rest/agile/1.0/backlog/issue", Some(&serde_json::json!({ "issues": [key.0] }))),
        }
    }

    /// Only `parent` goes: a whole-issue edit would re-send components Jira may have archived.
    fn set_epic(&self, key: &TicketKey, epic: &ConvoyKey) -> Result<()> {
        self.put_issue(key, &serde_json::json!({ "fields": { "parent": { "key": epic.0 } } }))
    }

    fn label(&self, key: &TicketKey, label: &str, on: bool) -> Result<()> {
        let op = if on { "add" } else { "remove" };
        self.put_issue(key, &serde_json::json!({ "update": { "labels": [{ op: label }] } }))
    }

    fn link(&self, key: &TicketKey, other: &TicketKey, link: TicketLink, on: bool) -> Result<()> {
        let issue = self.view(key)?;
        let fields = issue.get("fields").context("jira issue view: no fields")?;
        // Which side `other` is on: inward for "`other` blocks `key`", outward for "`key` duplicates
        // `other`", either for Relates.
        let (name, side) = match &link {
            TicketLink::BlockedBy => ("Blocks", Some(true)),
            TicketLink::Relates => ("Relates", None),
            TicketLink::Duplicates => ("Duplicate", Some(false)),
            TicketLink::Named(name) => (name.as_str(), Some(false)),
        };
        let ids: Vec<String> = links_of(fields, name)
            .into_iter()
            .filter(|l| &l.other == other && side.is_none_or(|inward| l.inward == inward))
            .map(|l| l.id)
            .collect();
        if !on {
            // By its id: `jira issue unlink A B` takes no link type, so it can drop a Relates for a Blocks.
            return ids.iter().try_for_each(|id| self.rest("DELETE", &format!("/rest/api/2/issueLink/{id}"), None));
        }
        if !ids.is_empty() {
            return Ok(()); // idempotent: the link exists
        }
        // `jira issue link INWARD OUTWARD Blocks` reads "INWARD blocks OUTWARD" (Jira's link
        // convention), so "key is blocked by `other`" is `link other key`. UNVERIFIED against a live
        // Jira. Relates reads the same both ways.
        match link {
            TicketLink::BlockedBy => self.jira(&["issue", "link", &other.0, &key.0, "Blocks"]),
            TicketLink::Relates => self.jira(&["issue", "link", &key.0, &other.0, "Relates"]),
            TicketLink::Duplicates => self.jira(&["issue", "link", &key.0, &other.0, "Duplicate"]),
            TicketLink::Named(name) => self.jira(&["issue", "link", &key.0, &other.0, &name]),
        }
        .map(drop)
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
fn detail_from_issue(issue: &serde_json::Value, points_field: Option<&str>) -> Option<TicketDetail> {
    let f = issue.get("fields")?;
    let names = |v: Option<&serde_json::Value>| v.and_then(|c| c.as_array()).into_iter().flatten().filter_map(|c| c["name"].as_str().map(String::from)).collect();
    Some(TicketDetail {
        ticket: issue_to_ticket(issue)?,
        // `issue view` says `issuetype`, `issue list` (a search) `issueType`.
        kind: f.pointer("/issuetype/name").or_else(|| f.pointer("/issueType/name")).and_then(|n| n.as_str()).unwrap_or("").to_lowercase(),
        description: match f.get("description") {
            Some(serde_json::Value::String(s)) => s.clone(),
            Some(doc) if doc.is_object() => adf_text(doc).trim_end().to_string(),
            _ => String::new(),
        },
        related: links_of(f, "Relates").into_iter().map(|l| l.other).collect(),
        duplicates: links_of(f, "Duplicate").into_iter().filter(|l| !l.inward).map(|l| l.other).collect(),
        links: named_links(f),
        components: names(f.get("components")),
        points: points_field.and_then(|id| f.get(id)).and_then(|p| p.as_f64()).map(|p| p.round() as u32),
        priority: f.pointer("/priority/name").and_then(|p| p.as_str()).map(String::from),
        fix_versions: names(f.get("fixVersions")),
        sprints: open_sprints(f),
        held: None,
        location: browse_url(issue).unwrap_or_default(),
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

/// The most issues jira-cli returns for one query: it never pages past the first.
const PAGE: usize = 100;

/// A page's issues: jira-cli prints `{issues:[…]}` or a bare array.
fn page_issues(json: &str) -> Result<Vec<serde_json::Value>> {
    let mut v: serde_json::Value = serde_json::from_str(json)?;
    let issues = if v.is_array() { v } else { v.get_mut("issues").map(serde_json::Value::take).unwrap_or_default() };
    serde_json::from_value(issues).context("expected {issues:[...]} (or an array) from jira")
}

/// A search's issues, all of them: a full page may not be, and jira-cli can't read past it, so
/// the query is refused rather than its matches cut short.
fn whole_page(json: &str, jql: &str) -> Result<Vec<serde_json::Value>> {
    let issues = page_issues(json)?;
    ensure!(issues.len() < PAGE, "{PAGE} or more tickets match {jql:?}, and jira-cli reads only the first {PAGE}: narrow the query");
    Ok(issues)
}

/// Parse one page → its tickets; unknown-status issues are dropped.
fn parse_page(json: &str) -> Result<Vec<TrackerTicket>> {
    Ok(page_issues(json)?.iter().filter_map(issue_to_ticket).collect())
}

fn issue_to_ticket(i: &serde_json::Value) -> Option<TrackerTicket> {
    let key = TicketKey::parse(i.get("key")?.as_str()?).ok()?;
    let f = i.get("fields")?;
    let status_at = |ptr: &str| f.get("status").and_then(|s| s.pointer(ptr)).and_then(|n| n.as_str()).unwrap_or("");
    Some(TrackerTicket {
        key,
        title: f.get("summary").and_then(|s| s.as_str()).unwrap_or("").to_string(),
        // A status of the project's own (Roll-out) reads as its category: To Do, In Progress or Done.
        status: jira_status(status_at("/name")).or_else(|| status_category(status_at("/statusCategory/key")))?,
        assignee: f
            .get("assignee")
            .and_then(|a| a.get("displayName"))
            .and_then(|n| n.as_str())
            .filter(|n| !n.is_empty()) // a search says an unassigned ticket's name is ""
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
        labels: f
            .get("labels")
            .and_then(|l| l.as_array())
            .map(|l| l.iter().filter_map(|x| x.as_str().map(String::from)).collect())
            .unwrap_or_default(),
    })
}

/// A ticket's blockers from its `issueLinks`: the inward side of a "Blocks" link — "X is blocked by
/// Y" ⇒ a `Blocks`-type link on X carrying `inwardIssue` = Y. The outward "blocks" side is the
/// reverse edge (carried on the blocker, not here); `Relates` / "Work item split" / etc. are ignored.
/// NB: jira-cli spells the field two ways — `issue list --raw` says `issueLinks`, `issue view
/// --raw` (Jira's own REST shape) says `issuelinks`.
fn parse_blocked_by(fields: &serde_json::Value) -> Vec<TicketKey> {
    links_of(fields, "Blocks").into_iter().filter(|l| l.inward).map(|l| l.other).collect()
}

/// One of an issue's links, seen from the issue: its id (what a delete takes), the issue on the
/// other end, and whether that one is the inward side ("Y blocks X", on X, carries `inwardIssue` = Y).
struct IssueLink {
    id: String,
    other: TicketKey,
    inward: bool,
}

/// The issue's links of type `name` (`Blocks`, `Relates`), from its `issueLinks` / `issuelinks`.
fn links_of(fields: &serde_json::Value, name: &str) -> Vec<IssueLink> {
    let links = fields.get("issueLinks").or_else(|| fields.get("issuelinks"));
    links
        .and_then(|l| l.as_array())
        .into_iter()
        .flatten()
        .filter(|l| l.pointer("/type/name").and_then(|n| n.as_str()) == Some(name))
        .filter_map(|l| {
            let (side, inward) = match l.get("inwardIssue") {
                Some(i) => (i, true),
                None => (l.get("outwardIssue")?, false),
            };
            Some(IssueLink {
                id: l.get("id").and_then(|i| i.as_str()).unwrap_or_default().to_string(),
                other: TicketKey::parse(side.get("key")?.as_str()?).ok()?,
                inward,
            })
        })
        .collect()
}

/// The issue's links of types the fleet doesn't act on, each as the issue reads it: its outward
/// verb ("split to") where it's the outward end, its inward one ("split from") where it's the other.
fn named_links(fields: &serde_json::Value) -> Vec<domain::NamedLink> {
    let links = fields.get("issueLinks").or_else(|| fields.get("issuelinks"));
    links
        .and_then(|l| l.as_array())
        .into_iter()
        .flatten()
        .filter(|l| !matches!(l.pointer("/type/name").and_then(|n| n.as_str()), Some("Blocks" | "Relates" | "Duplicate") | None))
        .filter_map(|l| {
            let (other, verb) = match l.get("outwardIssue") {
                Some(o) => (o, l.pointer("/type/outward")),
                None => (l.get("inwardIssue")?, l.pointer("/type/inward")),
            };
            Some(domain::NamedLink { verb: verb?.as_str()?.to_string(), other: TicketKey::parse(other.get("key")?.as_str()?).ok()? })
        })
        .collect()
}

/// The sprints the issue is in that haven't closed: whichever field holds sprint objects (its id
/// differs per Jira instance).
fn open_sprints(fields: &serde_json::Value) -> Vec<String> {
    let sprints = fields.as_object().into_iter().flatten().filter_map(|(_, v)| v.as_array()).flatten();
    sprints
        .filter(|s| s.get("boardId").is_some() && s["state"].as_str().is_some_and(|st| st != "closed"))
        .filter_map(|s| s["name"].as_str().map(String::from))
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

/// A Jira status category → the `TicketStatus` it stands for.
fn status_category(key: &str) -> Option<TicketStatus> {
    match key {
        "new" => Some(TicketStatus::Todo),
        "indeterminate" => Some(TicketStatus::InProgress),
        "done" => Some(TicketStatus::Done),
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
        let d = detail_from_issue(&issue, None).unwrap();
        assert_eq!((d.ticket.key.0.as_str(), d.kind.as_str()), ("PROJ-7", "story"));
        assert_eq!(
            d.description,
            "## Outcome\n\n**`neg`** works (PROJ-2), per [the spec](https://x.io/s).\n\n- AC one\n\n1. first\n2. second\n\n```rust\nlet x = 1;\n```\n\n> quoted"
        );
        assert_eq!(capitalized("story"), "Story");
        assert_eq!(d.location, "https://example.atlassian.net/browse/PROJ-7");
        assert_eq!(d.ticket.labels, vec!["fleet-proposed".to_string()]);
        assert_eq!(d.ticket.blocked_by, vec![TicketKey("PROJ-3".into())], "view --raw spells it `issuelinks`");
    }

    #[test]
    fn a_search_reads_one_page_and_refuses_a_full_one() {
        let page = |n: usize| serde_json::json!({ "issues": vec![serde_json::json!({ "key": "PROJ-1" }); n] }).to_string();
        assert_eq!(whole_page(&page(PAGE - 1), "text ~ x").unwrap().len(), PAGE - 1);
        assert_eq!(whole_page("[]", "text ~ x").unwrap().len(), 0);
        let full = whole_page(&page(PAGE), "text ~ x").unwrap_err().to_string();
        assert!(full.contains("narrow the query"), "{full}");
        assert!(page_issues(r#"{"total": 3}"#).is_err() && page_issues(r#""no""#).is_err());
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
        let ts = parse_page(SAMPLE).unwrap();
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
        let ts = parse_page(LINKED).unwrap();
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
    fn reads_relates_links_either_way_with_their_ids() {
        let issue: serde_json::Value = serde_json::from_str(r#"{"key":"PROJ-1","fields":{"summary":"s","status":{"name":"To Do"},
          "issuelinks":[
            {"id":"10","type":{"name":"Relates"},"inwardIssue":{"key":"PROJ-2"}},
            {"id":"11","type":{"name":"Relates"},"outwardIssue":{"key":"PROJ-3"}},
            {"id":"12","type":{"name":"Blocks"},"inwardIssue":{"key":"PROJ-4"}},
            {"id":"13","type":{"name":"Blocks"},"outwardIssue":{"key":"PROJ-5"}}]}}"#).unwrap();
        let d = detail_from_issue(&issue, None).unwrap();
        assert_eq!(d.related, vec![TicketKey("PROJ-2".into()), TicketKey("PROJ-3".into())], "either side");
        assert_eq!(d.ticket.blocked_by, vec![TicketKey("PROJ-4".into())], "only the inward Blocks side");
        let blocks = links_of(&issue["fields"], "Blocks");
        let ids: Vec<(&str, bool)> = blocks.iter().map(|l| (l.id.as_str(), l.inward)).collect();
        assert_eq!(ids, vec![("12", true), ("13", false)], "the id a delete takes, and the side");
    }

    #[test]
    fn status_mapping_round_trips_and_handles_in_review() {
        assert_eq!(jira_status(jira_status_name(TicketStatus::InQa)), Some(TicketStatus::InQa));
        assert_eq!(jira_status("In Review"), Some(TicketStatus::InQa)); // board label
        assert_eq!(jira_status("Nope"), None);
        assert_eq!(status_category("indeterminate"), Some(TicketStatus::InProgress)); // Roll-out
        assert_eq!(status_category("nope"), None);
    }

    #[test]
    fn a_detail_carries_components_points_duplicates_and_a_status_by_its_category() {
        let issue = serde_json::json!({
            "key": "PROJ-6", "self": "https://x.atlassian.net/rest/api/2/issue/1",
            "fields": {
                "summary": "Roll it out", "issueType": {"name": "Task"}, "assignee": {"displayName": ""},
                "status": {"name": "Roll-out", "statusCategory": {"key": "indeterminate"}},
                "components": [{"id": "1", "name": "docs"}, {"id": "2", "name": "services"}],
                "customfield_10004": 3.0,
                "priority": {"name": "Major"},
                "fixVersions": [{"id": "5", "name": "MC-V1-GA"}],
                "customfield_10007": [{"id": 1, "name": "Sprint 19", "state": "closed", "boardId": 7}, {"id": 2, "name": "Sprint 20", "state": "active", "boardId": 7}],
                "issuelinks": [
                    {"id": "9", "type": {"name": "Duplicate"}, "outwardIssue": {"key": "PROJ-2"}},
                    {"id": "8", "type": {"name": "Duplicate"}, "inwardIssue": {"key": "PROJ-3"}},
                    {"id": "7", "type": {"name": "Work item split", "inward": "split from", "outward": "split to"}, "outwardIssue": {"key": "PROJ-4"}},
                    {"id": "6", "type": {"name": "Documentation", "inward": "is documented by", "outward": "documents"}, "inwardIssue": {"key": "PROJ-5"}}
                ]
            }
        });
        let d = detail_from_issue(&issue, Some("customfield_10004")).unwrap();
        assert_eq!(d.ticket.status, TicketStatus::InProgress, "a status of the project's own reads as its category");
        assert_eq!((d.components, d.points), (vec!["docs".to_string(), "services".to_string()], Some(3)));
        assert_eq!(d.duplicates, vec![TicketKey("PROJ-2".into())], "only what it duplicates, not what duplicates it");
        assert_eq!(d.kind, "task", "a search's issueType, as a view's issuetype");
        assert_eq!((d.priority.as_deref(), d.fix_versions.as_slice()), (Some("Major"), ["MC-V1-GA".to_string()].as_slice()));
        assert_eq!(d.sprints, ["Sprint 20"], "only the sprints that haven't closed");
        let link = |verb: &str, k: &str| domain::NamedLink { verb: verb.into(), other: TicketKey(k.into()) };
        assert_eq!(d.links, [link("split to", "PROJ-4"), link("is documented by", "PROJ-5")], "each as the issue reads it");
        assert_eq!(d.ticket.assignee, None, "a search's empty name is unassigned");
        assert_eq!(detail_from_issue(&issue, None).unwrap().points, None, "no points field configured");
    }
}
