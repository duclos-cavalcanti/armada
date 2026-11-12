---
description: >-
  Report a convoy's progress as one self-contained HTML page: how far it has moved (a % grade, the
  convoy's and its critical path's), what it has cost so far (USD by role), a board of its tickets (To-Do, In-Progress, In-QA, Blocked,
  Done; the critical path's in red, and a diagram of it) and the proposals waiting to join it, its plan (folded), then each column's tickets linked to the tracker: what the blocked ones wait
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

It prints the page's path (default `reports/<p>/convoys/<convoy-title>_<UTC date>.html`; a
second run that day replaces it), then the summary as JSON. Open the page (`open <path>`).

- `scripts/report.sh` gathers: `fleetc context --convoy` (each ticket's status, open blockers, hold,
  live cog, MR state, critical path, as fleetd derives them), the ledger read-only
  (`scripts/ledger.sql`: titles, each ticket's latest MR, the tickets worked by hand, the open
  proposals naming the convoy), the convoy's spend (`/fleet-cost-report`'s `cost.sql` over the
  telemetry, read-only) and the tracker's link (`fleetc ticket --where`).
- `scripts/summary.jq` stages, flags and grades the tickets.
- `template.html` renders them; the data rides in it as JSON.

**The grade.** Each ticket weighs by its stage: To-Do 0, In-Progress ⅓, In-QA ⅔, Done 1, averaged
over the convoy's tickets. Cancelled ones are left out.

**The critical path.** The convoy's goal (`fleetc convoy open --goal`, the ticket whose Done proves
its outcome) and every ticket of the convoy it waits on, done or not, as fleetd derives it
(`critical` in `fleetc context`). It gets its own grade, and its tickets sit in red at the foot of
each column, and a diagram of it (folded): the path's tickets and their edges, the goal ringed in
red, each blocker in another convoy dashed with its number. The page is self-contained but for that
diagram: opening it loads mermaid (pinned) from jsDelivr; offline, it shows the diagram's source. A
convoy with no goal has none.

**Escalations.** Each escalation of a convoy ticket's cog, as its inspection judged it: refuted (an
option cleared it, sent back to the cog) or confirmed (it went to the operator), with their shares,
and the rulings made on the convoy's cases.

**The spend.** What the convoy has cost so far, by role: every request its captain and its tickets'
cogs made, their subagents included, attributed as `/fleet-cost-report` does (one query, shared).

**The board.** Each ticket sits in one column: To-Do, In-Progress, In-QA, Blocked, Done; they add up
to Tickets. **Blocked** is an open ticket that waits on an open blocker or is Blocked on the tracker;
it leaves its stage's column (the grade still counts its stage). **Proposed** is the open proposals
naming the convoy: outside it until accepted, so never in Tickets or the grade. **By hand** means
marked for a hand, dispatched to one, or worked by one.

## Hand off

The path, then one line: `#N KEY: G% moved (C% critical path, d/c Done) · D/T Done · Q In-QA · P
In-Progress · B Blocked · H by hand · R proposed · E escalations, F% refuted · $U spent`. Add one more line naming what holds the convoy back most: the blocker the most tickets wait
on, or the hold that keeps the most tickets off the frontier. That line is your judgement; the page
is measured.
