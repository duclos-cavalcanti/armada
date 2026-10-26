---
description: Classify a broken GitLab pipeline on the current branch and attempt a good-faith fix (retry/lint/build/test/infra); escalate if unclassifiable or out of scope
argument-hint:
---

# Skill: fleet-repair

You are attempting a good-faith fix for a broken GitLab pipeline on the current
branch's MR. The skill discovers the MR and its latest pipeline from the current
branch; if the pipeline is healthy (`success` / `running` / pending / etc.), the
skill no-ops and exits 0. If `failed` or `canceled`, the skill investigates via
`/ci-failures`, classifies the failure, and either commits + pushes a
category-appropriate fix or escalates.

**Preconditions** (caller's responsibility): clean working tree, worktree
checked out on the MR's branch, local tree mirrors origin.

This skill ends with **either** a committed + pushed fix (the new pipeline that
push triggers will be observed by the caller's next tick), **or** no-op exit 0
when the pipeline is already healthy, **or** an escalation when the failure
isn't classifiable or a fix would exceed ticket scope.

**Persona:** You are a Senior Staff engineer with Rust, CDK, and CI familiarity.
Your goal is to land a fix that turns the failing pipeline green without
expanding scope beyond the ticket. If uncertain, escalate rather than improvise.

---

## Lifecycle at a glance

- **Discover and gate** — find the current branch's MR + latest pipeline; no-op
  if healthy
- **Investigate** — invoke `/ci-failures` to surface failing jobs and traces
- **Gather context** — ticket ACs (Jira), MR description, current code state
- **Classify and fix** — match the failure to a category, apply the action,
  commit + push; or escalate if unclassifiable / out-of-scope

---

## Step 0 — Discover and gate

```bash
set -euo pipefail

# Find the ticket's open MR (fleetd's packet). Empty = no open MR → nothing to fix.
MR_IID=$(fleetc packet "$FLEET_TICKET" | jq -r '.ship.iid // empty')
[ -n "$MR_IID" ] || { echo "no open MR for $FLEET_TICKET" >&2; exit 1; }

# Fetch the latest pipeline for the MR. The ${MR_IID} suffix keeps concurrent
# cogs from clobbering each other's tempfile in the shared /tmp.
glab api "projects/:fullpath/merge_requests/${MR_IID}/pipelines" > "/tmp/fleet-repair-pipelines-${MR_IID}.json"
PIPELINE_STATUS=$(python3 -c 'import json,sys; ps=json.load(sys.stdin); print(ps[0]["status"] if ps else "none")' < "/tmp/fleet-repair-pipelines-${MR_IID}.json")
PIPELINE_ID=$(python3 -c 'import json,sys; ps=json.load(sys.stdin); print(ps[0]["id"] if ps else "")' < "/tmp/fleet-repair-pipelines-${MR_IID}.json")

# Gate: only proceed on broken pipelines; no-op gracefully on healthy ones.
case "$PIPELINE_STATUS" in
  failed|canceled) ;;  # fall through to fix
  success|running|pending|created|preparing|manual|scheduled)
    echo "MR !${MR_IID} latest pipeline (${PIPELINE_ID}) is ${PIPELINE_STATUS} — nothing to fix"
    exit 0
    ;;
  none) echo "no pipeline yet for MR !${MR_IID}"; exit 1 ;;
  *) echo "unexpected pipeline status: $PIPELINE_STATUS"; exit 1 ;;
esac
```

Then invoke `/ci-failures` with `$PIPELINE_ID` via the Skill tool. Capture:

- Failing jobs (names, stages)
- Failure traces (compiler/test output, exit codes)
- Any classification cues `/ci-failures` surfaces

---

## Step 1 — Gather context

Three inputs inform the fix:

- **Ticket ACs** — fetch via `mcp__claude_ai_Atlassian_Rovo__getJiraIssue` if not
  already in conversation. The ticket key is `$FLEET_TICKET` (fleetd injects it into
  every ticketed cog).
- **MR description + scope decisions** — `glab api
  "projects/:fullpath/merge_requests/${MR_IID}"` (the `MR_IID` discovered in
  Step 0).
- **Current code state** — the local tree (mirrored by `fleet-rechart`).

---

## Step 2 — Classify and fix

Match the failure to one of the rows below. **One fix attempt per skill
invocation** — the push triggers a new pipeline; the caller's next tick handles
whatever state it produces.

| Category | Cue from `/ci-failures` | Action |
|---|---|---|
| **Transient** | Runner death, infra flake, network blip, image-pull timeout, OOM, sccache miss/disk pressure, intermittent test that's been flaky | Branch / merge-result ref → `glab ci retry $PIPELINE_ID`. Train ref (`refs/merge-requests/<iid>/train`) → that endpoint 404s; retry the failing job(s) directly via `glab api --method POST projects/:fullpath/jobs/<job_id>/retry`. No commit either way. |
| **Lint / format** | `cargo fmt` drift, `cargo clippy -- -D warnings`, `shellcheck`, markdown lint | Run the formatter / lint-fixer against affected files. `git commit -m "fix: <linter> auto-fix"`. Push. |
| **Build** | Compile error, syntax error, missing dependency, broken import path | Read the failure trace, identify the broken file, fix it (direct edit informed by ticket context). Commit + push. |
| **Test** | Unit / integration / contract / robustness test failed | Diagnose: *test is wrong* (missing edge case → spawn `tdd-red`) vs *implementation is wrong* (impl gap → spawn `tdd-green`). Commit per phase, push. **If diagnosis is uncertain — the failure trace and ticket context don't make the wrong-side obvious — escalate rather than guess.** |
| **Infra** | `cdk synth --strict` violation, aspect failure, IAM denial on deploy | Fix the CDK / IAM / aspect issue against `MANIFEST.md` § Ephemeral Invariants. Set up the CDK venv if not already active (`python -m venv .venv && source .venv/bin/activate && pip install -r infra/cdk/requirements.txt`), then validate locally with `cdk synth --strict`. Commit + push. |
| **Unknown / Major / Out-of-scope** | Failure trace doesn't fit a category above; fix would require architectural decisions; fix would touch files outside the ticket's scope; multi-MR fix needed | **Escalate** — see Failure handling. |

**Self-reflection.** After the fix attempt, post a Jira comment summarising:
failure category, what was tried, commit SHA pushed (if any), and the new
pipeline ID once one starts.

---

## Failure handling

If the failure is unclassifiable, requires architectural decisions, or would
touch files outside the ticket's scope, write a summary (failure trace +
classification attempt + why a fix would exceed scope) to **both** the Jira
ticket *and* your claude-code response, then exit non-zero. The caller
(the sailor) propagates this upward.
