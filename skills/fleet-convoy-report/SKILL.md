---
description: >-
  Report a convoy's progress as one self-contained HTML page: its tickets by stage (done, in QA,
  in progress, to do), how far it has moved (a % grade), the blocked ones and what they wait on, the
  anchored ones and why, and the ones worked by hand. Read-only. For the shipwright, on the
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

It prints the page's path (default `projects/<p>/exports/convoy-<N>-<UTC stamp>.html`), then the
summary as JSON. Open the page (`open <path>`).

- `scripts/report.sh` gathers: `fleetc context --convoy` (each ticket's status, open blockers, hold,
  live cog, MR state, as fleetd derives them), the ledger read-only (`scripts/ledger.sql`: titles,
  each ticket's latest MR, the tickets worked by hand) and the tracker's link (`fleetc ticket --where`).
- `scripts/summary.jq` stages, flags and grades the tickets.
- `template.html` renders them; the data rides in it as JSON.

**The grade.** Each ticket weighs by its stage: To do 0, In progress ⅓, In QA ⅔, Done 1, averaged
over the convoy's tickets. Cancelled ones are left out. **Blocked** means it waits on an open blocker
or is Blocked on the tracker. **By hand** means marked for a hand, dispatched to one, or worked by one.

## Hand off

The path, then one line: `#N KEY: G% moved · D/T done · Q in QA · P in progress · B blocked · H by
hand`. Add one more line naming what holds the convoy back most: the blocker the most tickets wait
on, or the hold that keeps the most tickets off the frontier. That line is your judgement; the page
is measured.
