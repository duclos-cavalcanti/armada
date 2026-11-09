---
description: >-
  Report a convoy's progress as one self-contained HTML page: how far it has moved (a % grade), a
  board of its tickets (To-Do, In-Progress, In-QA, Blocked, Done) and the proposals waiting to join
  it, its plan (folded), then each column's tickets linked to the tracker: what the blocked ones wait
  on, why the anchored ones are held, which are worked by hand. Read-only. For the shipwright, on the
  operator's order.
argument-hint: "<convoy KEY|#N> [projects/<p>]"
allowed-tools: Bash(${CLAUDE_SKILL_DIR}/scripts/report.sh *)
---

# Skill: fleet-convoy-report

One page on where a convoy stands, generated from the fleet's own state. Scripted end to end, so
two runs on the same state give the same page.

## Run

```bash
${CLAUDE_SKILL_DIR}/scripts/report.sh projects/<p> <KEY|#N> [<out.html>]
```

It prints the page's path (default `projects/<p>/exports/<convoy-title>_<UTC date>.html`; a
second run that day replaces it), then the summary as JSON. Open the page (`open <path>`).

- `scripts/report.sh` gathers: `fleetc context --convoy` (each ticket's status, open blockers, hold,
  live cog, MR state, as fleetd derives them), the ledger read-only (`scripts/ledger.sql`: titles,
  each ticket's latest MR, the tickets worked by hand, the open proposals naming the convoy) and the
  tracker's link (`fleetc ticket --where`).
- `scripts/summary.jq` stages, flags and grades the tickets.
- `template.html` renders them; the data rides in it as JSON.

**The grade.** Each ticket weighs by its stage: To-Do 0, In-Progress ⅓, In-QA ⅔, Done 1, averaged
over the convoy's tickets. Cancelled ones are left out.

**The board.** Each ticket sits in one column: To-Do, In-Progress, In-QA, Blocked, Done; they add up
to Tickets. **Blocked** is an open ticket that waits on an open blocker or is Blocked on the tracker;
it leaves its stage's column (the grade still counts its stage). **Proposed** is the open proposals
naming the convoy: outside it until accepted, so never in Tickets or the grade. **By hand** means
marked for a hand, dispatched to one, or worked by one.

## Hand off

The path, then one line: `#N KEY: G% moved · D/T Done · Q In-QA · P In-Progress · B Blocked · H by
hand · R proposed`. Add one more line naming what holds the convoy back most: the blocker the most tickets wait
on, or the hold that keeps the most tickets off the frontier. That line is your judgement; the page
is measured.
