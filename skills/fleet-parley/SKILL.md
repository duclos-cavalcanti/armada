---
description: Answer the open review threads on the ticket's MR — validate each finding, fix it your way or push back, reply, resolve — through `fleetc review` / `fleetc reply` (forge-neutral). Count-gated, and surfaces floating review notes before declaring the MR clean.
argument-hint:
---

# Skill: fleet-parley

You are answering the reviewers on the ticket's MR. For each open thread: **validate the
finding, then derive and apply the fix yourself**, or push back when it's wrong. Reply on
the thread and resolve it: once you've answered, it's the reviewer's to re-open. The reviewer
diagnoses; you own the correction.

**Ends with** every addressable thread handled (exit 0, `OPEN=N REVIEW_NOTES=M` on
stdout), or an escalation when a finding needs the operator (exit 1).

## Step 0 — Read the review and gate

```bash
set -euo pipefail
PACKET=$(fleetc packet)
[ -n "$(printf '%s' "$PACKET" | jq -r '.ship.iid // empty')" ] || { echo "no open MR" >&2; exit 1; }
# Gates nit deferral: at least one approval, and all that are required.
APPROVED=$(printf '%s' "$PACKET" | jq '(.snapshot.approvals // {current: 0}) | .current > 0 and .current >= .required')
echo "APPROVED=$APPROVED"
REVIEW=$(fleetc review)   # your MR's: {open_threads, threads: [{id, path, line, author, body}], floating_notes}
OPEN=$(printf '%s' "$REVIEW" | jq '.open_threads')
REVIEW_NOTES=$(printf '%s' "$REVIEW" | jq '.floating_notes | length')
if [ "$OPEN" -eq 0 ]; then
  echo "OPEN=0 REVIEW_NOTES=$REVIEW_NOTES"
  printf '%s' "$REVIEW" | jq -r '.floating_notes[] | "  floating-note [\(.created_at)] \(.author): \(.body[0:240])"'
  exit 0
fi
printf '%s' "$REVIEW" | jq -r '.threads[] | "== \(.id)  \(.path // "-"):\(.line // "-")  @\(.author)\n\(.body)\n"'
```

**Floating notes** are review comments that aren't threads (a reviewer's summary or HOLD).
They don't block the merge, so they aren't in `OPEN`, but they can gate an approval:
read them, and never treat `OPEN=0` as "review complete" while one asks for changes.

The scope authority is the **ticket** (`fleetc ticket`): its acceptance
criteria and its *Out of scope*.

## Step 1 — Classify each thread

Start strict, soften down: walk the categories in order; the first that clearly fires wins.

1. **Escalate** — you can't answer it; the operator rules:
   - **Two reviewers conflict** on the same lines; both can't be satisfied.
   - **It would amend an acceptance criterion, a design decision, or an invariant**: that's
     the ticket author's call, not a patch.
   **Push back** — you answer it with evidence (only on a clear match; a partial match
   softens to 3):
   - **A factual error or false diagnosis**: the code, a grep, or the project's doctrine
     contradicts what the reviewer asserts, or the violated property doesn't actually hold.
     The reply cites the evidence.
   - **Scope expansion**: the ask goes beyond the ticket (tightening, tests, better
     errors, and lint stay *inside* scope). Propose it (`/fleet-propose`); the reply names
     the proposed ticket's key.
2. **Defer a nit** (only when `APPROVED` is true): the MR has its approvals, and a push can
   reset them, costing a review round for polish. A nit is what the reviewer labels one
   (`nit`, `[nit]`), or a pure style / naming / polish ask that changes no behaviour,
   correctness, test, or doc accuracy. When in doubt, it isn't a nit: apply it.
3. **Apply with research**: the ask is underspecified ("this might break on X", "consider
   Y"). Verify or compare, then apply; the reply says what you chose and why.
4. **Apply** (the default): confirm the diagnosis against the current code, derive the fix
   (the reviewer's suggestion is one input, not an instruction), apply it.
   - **Right problem, wrong remedy**: apply your own fix; the reply says both.
5. **Asks nothing** (a verdict, a pass, an acknowledgement): reply with the disposition and
   resolve it now.

When a push-back trigger is a genuine 50/50, descend to 3 and note the ambiguity in the
reply. Never answer a correct finding with "we'll ticket it later": a right finding is
fixed now, a wrong one is pushed back. The one exception is a nit on an approved MR (2).

## Step 2 — Fix, verify, push

Make the fixes in the worktree, run the tests, commit, and push. After a rebase, push with
`--force-with-lease`, never plain `--force`.

## Step 3 — Reply and resolve

**Deferred nits first:** propose them all as **one** follow-up ticket with `/fleet-propose`
(a task: "Nits from !<iid>", one bullet per thread: its file:line and ask). Then reply on each
nit thread "Deferred to <KEY>: the MR is approved, and polish isn't worth resetting its
approvals." and resolve it.

For each other thread you handled:

```bash
fleetc reply --thread <id> --resolve --body - <<'EOF'
<what you changed and why — or, for "right problem, wrong remedy", the diagnosis you
accepted and the fix you chose instead>
EOF
```

**Resolve every thread you answer**: a fix, a deferral, a push-back with its evidence. Answering
is your job; judging the answer is the reviewer's, and one who disagrees re-opens the thread.
An ask for evidence CI produces (a test that must pass, a job that must go green) is answered
by the fix and a reply naming that job: resolve it. The pipeline gates the merge on its own,
green or red, so an open thread would only be a second gate on your sweep and your arm. Leave
open only what you can't answer yet: an escalation, until the operator rules.

## Step 4 — Re-read and report

Your push may have answered a floating note, or a new verdict may have landed, so re-read
against the new HEAD:

```bash
REVIEW=$(fleetc review)
echo "OPEN=$(printf '%s' "$REVIEW" | jq '.open_threads') REVIEW_NOTES=$(printf '%s' "$REVIEW" | jq '.floating_notes | length')"
printf '%s' "$REVIEW" | jq -r '.floating_notes[] | "  floating-note [\(.created_at)] \(.author): \(.body[0:240])"'
```

| Outcome | Action | Exit |
|---|---|---|
| Every thread handled, fixes pushed | the `OPEN=N REVIEW_NOTES=M` line above | 0 |
| An escalation | report the thread, what it asks, and the ruling it needs, in your response | 1 |
| Tests fail or the push is refused | report the error in your response | 1 |

The caller (the sailor) reads `OPEN` (the merge-blocking threads) and `REVIEW_NOTES` (read
them before treating the MR as clean), and escalates on exit 1.
