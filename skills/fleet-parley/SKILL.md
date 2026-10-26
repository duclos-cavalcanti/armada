---
description: Address open review threads on the current branch's MR via /mr-reply, with count-gating and outcome propagation
argument-hint:
---

# Skill: fleet-parley

You are addressing open reviewer findings on the current branch's MR. The skill
discovers the MR from the current branch and wraps the team-standard `/mr-reply`
with three additions: a count-gate (skip invocation if there are no open
threads), outcome propagation (map `/mr-reply`'s four outcomes to exit codes),
and a post-invocation re-count (which the caller reads to gate downstream
auto-merge).

This skill ends with **either** all addressable threads handled and exit 0 (with
`OPEN=N` printed on stdout), **or** an escalation when `/mr-reply` surfaces
strong pushback / force-push prompt / generic failure (exit 1, dual-channel
surface).

---

## Lifecycle at a glance

- **Count open threads + fetch floating notes** — even at `OPEN=0`, surface the
  floating review notes and emit `REVIEW_NOTES=M` before exiting 0 (never a bare
  no-op — a HOLD gates approval, not merge)
- **Delegate to `/mr-reply`** — apply fixes, verify, rebase, push (the heavy
  work)
- **Propagate outcome** — map to exit code; re-count threads + re-surface
  floating review notes; emit `OPEN=N REVIEW_NOTES=M` on stdout

---

## Step 0 — Count open threads

```bash
set -euo pipefail

# Find the ticket's open MR (fleetd's packet), then read its fresh review surface via fleetc.
# `fleetc review` is one call to the factory's forge that returns BOTH the merge-blocking
# open-thread count (resolvable && !resolved && !system) AND the floating notes the count
# EXCLUDES (!resolvable && !system) — the same structural filters that kept the bash
# count-gate false-positive-free, now homed in forge.rs (not re-implemented inline).
MR_IID=$(fleetc packet "$FLEET_TICKET" | jq -r '.ship.iid // empty')
[ -n "$MR_IID" ] || { echo "no open MR for $FLEET_TICKET" >&2; exit 1; }
REVIEW=$(fleetc review --iid "$MR_IID")
OPEN=$(printf '%s' "$REVIEW" | jq '.open_threads')

# The floating notes the count EXCLUDES. A reviewer HOLD or an un-actioned should-fix/must-fix
# is frequently a floating `individual_note` / `/mr-review` summary: GitLab doesn't gate the
# *merge* on it (so OPEN ignores it, correctly) but it gates the *approval* you're waiting on.
# `fleetc review` carries those notes' CONTENT (detection here, judgment is yours). OPEN=0 is NOT
# "review-complete" while REVIEW_NOTES>0: read them and address any blocking verdict at/after HEAD.
FLOATING=$(printf '%s' "$REVIEW" | jq '.floating_notes')
REVIEW_NOTES=$(printf '%s' "$FLOATING" | jq 'length')

if [ "$OPEN" -eq 0 ]; then
  echo "OPEN=0 REVIEW_NOTES=$REVIEW_NOTES"
  if [ "$REVIEW_NOTES" -gt 0 ]; then
    # Surface them so the caller (sailor / self-review) evaluates before
    # treating the MR as clean — do NOT silently conclude "just awaiting approval".
    printf '%s\n' "$FLOATING" | jq -r '.[] | "  floating-note [\(.created_at)] \(.author): \(.body[0:240])"'
  fi
  exit 0
fi
```

---

## Step 1 — Delegate to /mr-reply

Invoke `/mr-reply` via the Skill tool: `Skill(skill="mr-reply",
args="$MR_IID")`. The team-standard skill handles the heavy work — fetch
findings, classify (strong-pushback / apply-with-research / apply), apply fixes,
verify, rebase against the upstream target, push.

`/mr-reply` is idempotent: already-resolved threads are filtered out, so
re-firing on a strong-pushback iteration is harmless.

One thread rule this wrapper enforces around the delegation — read each
thread's BODY, never just the header counters, and split on whether it **asks
anything of the MR**:

- **Asks nothing** (a verdict, a pass receipt, an acknowledgement — e.g. a
  review bot's "Ship, resolve to unblock merge") → reply with the disposition
  and **resolve it this pass**; an open no-ask thread only gates the merge
  train for no reason.
- **Asks something** (a fix, a change, a question) → engage it — fix it or
  answer it in-thread — and resolve only once every ask is addressed. Never
  resolve over an unanswered question; that silently discards the reviewer's
  ask.

---

## Step 2 — Propagate outcome

Map `/mr-reply`'s outcome to this skill's exit code:

| `/mr-reply` outcome | This skill's action | Exit |
|---|---|---|
| Posted replies, all threads resolved, push succeeded | Re-count threads + re-surface floating notes; emit `OPEN=N REVIEW_NOTES=M` on stdout | 0 |
| Returned a `STRONG-PUSHBACK` block | Write the block to **both** the Jira ticket *and* claude-code response | 1 |
| Blocked asking for force-push approval | Write the prompt to **both** channels — the user approves manually in the foreground | 1 |
| Failed (verification gate, push error, etc.) | Write the error to **both** channels | 1 |

On the success path — this is the **post-reply terminal** check. `/mr-reply` may
have pushed commits (new HEAD), so re-evaluate floating notes HERE, against the
current HEAD: a HOLD checked pre-reply (Step 0) may be stale (your push just
addressed it), and a fresh reviewer verdict may have landed. Both terminal
`OPEN=0` exits — the Step 0 early-exit (started at 0) and this one (converged to
0 after the reply pass) — must surface `REVIEW_NOTES` so the caller never reads
`OPEN=0` as "review-complete" while a floating HOLD / should-fix is unaddressed:

```bash
REVIEW=$(fleetc review --iid "$MR_IID")
OPEN=$(printf '%s' "$REVIEW" | jq '.open_threads')
FLOATING=$(printf '%s' "$REVIEW" | jq '.floating_notes')
REVIEW_NOTES=$(printf '%s' "$FLOATING" | jq 'length')
echo "OPEN=$OPEN REVIEW_NOTES=$REVIEW_NOTES"
if [ "$REVIEW_NOTES" -gt 0 ]; then
  printf '%s\n' "$FLOATING" | jq -r '.[] | "  floating-note [\(.created_at)] \(.author): \(.body[0:240])"'
fi
```

The caller (the sailor) reads the `OPEN=N REVIEW_NOTES=M` line from this
stdout: `OPEN` gates the resolvable-thread merge-block, and `REVIEW_NOTES>0`
means "read the surfaced notes and judge before treating the MR as clean" — it
is NOT auto-merge-blocking (a floating note may be a clean APPROVE summary), it
is an evaluate-before-idle signal.

**Self-reflection.** Post a Jira comment summarising: number of threads
addressed, any STRONG-PUSHBACK encountered, any force-push prompt surfaced,
final `OPEN` count.

---

## Failure handling

The exit-1 paths above are the failure-handling contract — strong-pushback /
force-push-blocked / generic-failure all dual-channel surface + exit non-zero.
The caller propagates the exit upward.
