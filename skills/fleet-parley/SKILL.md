---
description: Answer the open review threads on the ticket's MR — treat each finding as false until you prove it against the code, docs and a run, then fix it your way (its remedy tested against every case), propose it when no AC of the ticket needs it, or push back with the evidence; escalate a finding that keeps coming back; defer nits to one follow-up ticket; reply, resolve — through `fleetc review` / `fleetc reply` (forge-neutral). Count-gated, and surfaces floating review notes before declaring the MR clean.
argument-hint:
---

# Skill: fleet-parley

You are answering the reviewers on the ticket's MR. A review is a **claim, not a fact**: for
each open thread, **prove the finding before you act on it**, then derive and apply the fix
yourself, or push back with the evidence that it's wrong. Reply on the thread and resolve it:
once you've answered, it's the reviewer's to re-open. The reviewer diagnoses; you verify, and
own the correction.

**Ends with** every addressable thread handled (exit 0, `OPEN=N REVIEW_NOTES=M` on
stdout), or an escalation when a finding needs the operator (exit 1).

## Step 0 — Read the review and gate

```bash
set -euo pipefail
PACKET=$(fleetc packet)
[ -n "$(printf '%s' "$PACKET" | jq -r '.ship.iid // empty')" ] || { echo "no open MR" >&2; exit 1; }
# An approved MR raises the bar: at least one approval, and all that are required.
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

## Step 1 — Prove each finding: false until proven

Treat every finding as **false until you prove it** against the ground truth:

- **the code** — at your HEAD and on `origin/HEAD`: read it, grep it, trace the path it names;
- **the docs** — the project's design records, doctrine and specs on `origin/HEAD`;
- **the ticket** (`fleetc ticket`) — its acceptance criteria and its *Out of scope*;
- **a run** — where the claim is testable, a test or command you execute.

Keep the evidence either way: `file:line`, the doc §, the command and what it printed.

- **Proven** → classify it below.
- **Disproven, or not proven** → **push back**: reply with what the code, the docs or the run
  actually show, and resolve. Never apply a finding you couldn't prove; never push back
  without evidence.

Then classify each proven finding — the first that fires wins:

1. **Escalate** — you can't answer it; the operator rules:
   - **Two reviewers conflict** on the same lines; both can't be satisfied.
   - **It would amend an acceptance criterion, a design decision, or an invariant**: that's
     the ticket author's call, not a patch.
   - **It keeps coming back**: this MR already fixed the same lines or rule for it twice
     (`git log --oneline -L<from>,<to>:<path> "origin/$(printf '%s' "$PACKET" | jq -r .ship.target)..HEAD"`
     shows the rewrites). Stop patching: reply "<n>th failure of this rule; needs a design
     ruling, escalated", and leave it open. Don't wait for the sailor's round cap.
2. **True, not this ticket** — ask whether one of *this* ticket's acceptance criteria needs it.
   If none does, it's a follow-up, however true: propose it (`/fleet-propose`), or add it to a
   follow-up this MR already proposed on the same subject. Reply "Proven, but outside <KEY>'s
   ACs (<the ACs you checked>). Proposed as <KEY-N>." and resolve. The exception: a proven
   correctness or security defect in what this MR itself adds is fixed here (Apply).
3. **Defer a nit** — always, approved MR or not: a nit is what the reviewer labels one (`nit`,
   `[nit]`), or a pure style / naming / polish ask that changes no behaviour, correctness, test,
   or doc accuracy. It goes to the MR's nits ticket (Step 3), never into this MR, even when it's
   a one-line fix.
4. **Apply** — the fix is yours, held to the same proof as the finding:
   - **Prove the remedy.** A reviewer's suggested fix or text is a second claim, and it can be
     wrong where the finding was right. Never paste it: write the fix from your own reading, and
     prove every statement your fix makes as you proved the finding. Test the remedy, the
     reviewer's or yours, against **every case** the rule or text covers, not just the one
     reported, and list the cases in the reply. **Right problem, wrong remedy**: "Right problem;
     your remedy fails <case> (<file:line>). Applied <X> instead."
   - **Fix the class.** A finding is one sighting of a mistake. Name the mistake (the wrong
     claim, assumption or shape), search your whole diff for it, and fix every instance now: the
     reply lists each site. Fix only the sighting, and the next round finds the rest one by one.
5. **Asks nothing** (a verdict, a pass, an acknowledgement): reply with the disposition and
   resolve it now.

**An approved MR** (`APPROVED=true`) pushes back harder: a push resets its approvals and costs a
review round. Apply only a proven defect — wrong behaviour, a correctness or security bug, an
acceptance criterion not met. Anything short of that (readability, a refactor, a test beyond
the ACs, doc polish) is deferred to the nits ticket like a nit.

A proven finding is fixed now, except: a nit, or a non-defect on an approved MR, is deferred;
one no AC needs is proposed; one that keeps coming back is escalated. An unproven one is pushed
back. Nothing is "ticketed for later" otherwise.

## Step 2 — Fix, verify, push

Only once every thread is settled (Step 1) and every fix is in the worktree: run the tests,
then commit and push **once**, never per thread. After a rebase, push with
`--force-with-lease`, never plain `--force`.

## Step 3 — Reply and resolve

**Deferred nits first:** they all go to **one** ticket per MR, "Nits from !<iid>" (a task, one
bullet per thread: its file:line and ask). If it's already proposed (`fleetc proposal` lists
it), add this round's bullets to it — `fleetc ticket edit <KEY> --body -`, starting from
`fleetc ticket <KEY>`, since it replaces the description whole. Otherwise propose it with
`/fleet-propose`. Then reply on each deferred thread "Deferred to <KEY>: <a nit | not a defect,
and the MR is approved>." and resolve it.

For each other thread you handled (a push-back cites its evidence):

```bash
fleetc reply --thread <id> --resolve --body - <<'EOF'
<what you changed and why, and every site of the class — or, for "right problem, wrong
remedy", the diagnosis you accepted and the fix you chose instead>
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
