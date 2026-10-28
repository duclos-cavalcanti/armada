---
description: Classify a broken CI run on the ticket's MR and attempt a good-faith fix (retry / lint / build / test / config); escalate if unclassifiable or out of scope. Forge-neutral — reads CI through `fleetc ci`.
argument-hint:
---

# Skill: fleet-repair

You are attempting a good-faith fix for broken CI on the ticket's MR. If CI is healthy
(passing or still running), the skill no-ops. If it failed, you read each failed job's
log, classify the failure, and either push a fix, re-run a transient failure, or
escalate.

**Preconditions** (the caller's): a clean working tree on the MR's branch, mirroring
origin.

**Ends with** a pushed fix (the next CI run is the caller's next look), a re-run, a no-op
when CI is healthy, or an escalation when the failure isn't classifiable or a fix would
exceed the ticket. Land CI green without widening scope; when unsure, escalate rather
than improvise.

## Step 0 — Read CI and gate

```bash
set -euo pipefail
CI=$(fleetc ci)          # your MR's CI: {status, failed: [{name, url, log_excerpt}]}
STATUS=$(printf '%s' "$CI" | jq -r '.status // "none"')
case "$STATUS" in
  failed|canceled) ;;                                    # fall through to the fix
  success|running|manual) echo "CI is $STATUS — nothing to fix"; exit 0 ;;
  none) echo "no CI yet"; exit 1 ;;
  *) echo "unexpected CI status: $STATUS"; exit 1 ;;
esac
printf '%s' "$CI" | jq -r '.failed[] | "== \(.name)  \(.url)\n\(.log_excerpt)\n"'
```

`fleetc ci` already descended into failed child pipelines (where the forge has them):
every failed job is listed, each with an excerpt of its log: the lines that read as a failure
(a job often runs on past its failure), then its tail. Not enough? Open the job's `url`.

## Step 1 — Gather context

- **The ticket** — `fleetc ticket`: the acceptance criteria bound what a
  fix may change.
- **The code** — the local tree, and the project's own conventions (`CLAUDE.md`, its
  `docs/`) for the area that failed.

## Step 2 — Classify and act

Read each failed job's log tail and name its root cause. **One fix attempt per
invocation**: the push starts a new CI run, and the caller's next look handles whatever
it produces.

| Category | Cue in the log | Action |
|---|---|---|
| **Transient** | A runner died, a network or registry timeout, out of memory, a cache miss, a test known to be flaky | `fleetc retry-ci` (its own command). No commit. |
| **Lint / format** | A formatter or linter diff (`cargo fmt`, `clippy -D warnings`, `shellcheck`, …) | Run the formatter / fixer on the affected files, commit (`fix: <linter> auto-fix`), push. |
| **Build** | A compile or syntax error, a missing dependency, a broken import | Read the error, fix the broken file within the ticket's scope, commit, push. |
| **Test** | A unit / integration test failed | Decide which side is wrong: the *test* (a missing case, a stale expectation) or the *code* (an implementation gap). Fix that side, commit, push. **If the log and the ticket don't make it obvious which side is wrong, escalate rather than guess.** |
| **Config** | The CI configuration itself: syntax, a missing stage or job, a wrong dependency between jobs | Fix the config per the project's conventions, commit, push. |
| **Unknown / out of scope** | The log fits none of the above; the fix needs a design decision, touches files outside the ticket, or spans MRs | **Escalate** (below). |

## Failure handling

When the failure is unclassifiable, needs a design decision, or a fix would leave the
ticket's scope: report the failed job, its log excerpt, your classification attempt, and
why a fix exceeds scope, in your response. Then exit non-zero; the caller (the sailor)
escalates.
