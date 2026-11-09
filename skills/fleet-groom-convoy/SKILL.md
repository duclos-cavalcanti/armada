---
description: >-
  Groom a whole convoy to Ready, as one: each open ticket meets the Definition of Ready (through the
  project's grooming skill, when it names one); together they cover the convoy plan's outcome and
  lead into each other; every blocker edge is needed and none is missing; and the critical path to
  the outcome is never held up by deferrable work (hardening, backfills, guard tests), which runs as
  a parallel stream after it. Reports first, then applies what the operator approves. For the
  admiral.
argument-hint: <KEY|#N> (the convoy)
---

# Skill: fleet-groom-convoy

A convoy can hold tickets that are each Ready and still not sail: a gap the plan's outcome needs and
no ticket owns, an edge that serialises work for nothing, a hardening ticket sitting in front of the
end-to-end proof. You groom the convoy as one, report what you find, and change nothing until the
operator says which of it to apply. You never dispatch: the captain does, from the frontier you
groomed.

## Step 0 — Ground

```bash
fleetc convoy plan <C>                        # the outcome, the sequencing, the rulings
fleetc context --convoy <C> --format table    # each ticket: status, dispatch verdict, held, live cog
fleetc ticket <KEY>                           # each open ticket in full: goal, ACs, blockers, links
fleetc escalations --key <KEY>                # the convoy's cases: rulings that bind its tickets
```

Read the canon where the plan points to it (`/fleet-bearings` if you hold none of it). A ticket
already dispatched is past grooming: leave its text alone, but count it in Steps 2 and 3.

## Step 1 — Each ticket Ready

Judge every open, undispatched ticket by the same bar the captain applies before dispatch, so a
groomed ticket passes it: the project's readiness check when it names one (`FLEET_ASSESS_SKILL`,
from `assess_skill` in its fleet.toml; read-only), else `/fleet-ready-done`'s Ready, and in either
case each AC against what the ticket's blockers deliver (`/fleet-ready-done`'s assess.md). A
convoy of many tickets: assess each in a fresh `lookout` subagent by that file. When the project names a grooming skill (`FLEET_GROOM_SKILL`, from
`groom_skill` in its fleet.toml), note each ticket it should run on: it runs in Step 4, since it
writes. A ticket anchored as not Ready that this grooming would make Ready is weighed back in Step 4;
one held by a convoy anchor ("convoy <C> anchored") is not yours to weigh.

## Step 2 — The convoy as one

- **Coverage.** Every part of the plan's outcome has a ticket that owns it. A part with none is a
  ticket to file; a ticket the outcome doesn't need belongs to another convoy, or nowhere.
- **Overlap.** No two tickets own the same behaviour or AC. Overlap is a merge or a split.
- **Flow.** Each ticket builds on what its blockers deliver and delivers what the next one needs:
  the chain from today's default branch to the outcome holds with no gap. The plan's sequencing
  says the same as the edges.
- **Rulings.** A ruling on one of the convoy's cases that binds a ticket is in its ACs.

## Step 3 — Edges and the critical path

Name each open ticket's role:

- **Crucial**: on the path to the outcome. The plan's proof that the feature works, often a live
  end-to-end ticket, needs it.
- **Deferrable**: needed, but not to prove the outcome: a backfill, a guard test against a past
  issue, a doc, a follow-up hardening.

Then weigh every `is blocked by` edge among the convoy's tickets (and each one into another convoy):

- **Needed**: the blocked ticket's ACs consume what the blocker delivers. Keep it.
- **Missing**: a ticket consumes what another delivers, with no edge. Add it.
- **Unneeded**: nothing in the blocked ticket's ACs depends on the blocker. Drop it: it only
  serialises work.
- **Deferrable before crucial**: a deferrable ticket blocks a crucial one. Drop it, even when the
  order feels natural. The deferrable ticket becomes a parallel stream: the captain dispatches it
  once it's Ready, beside the critical path, never in front of it. Keep it blocked by crucial work
  only where it truly consumes that work.
- **Cycles**: report them, and break each at its unneeded edge.

For example: A (a backfill) blocks B, and B is needed by C (the live end-to-end test). B and C are
crucial, A is deferrable. Drop A → B; A runs in parallel, and the plan lists it after the critical
path.

## Step 4 — Report, then apply

Report first, nothing written yet:

```
Convoy <C> — <title> · <n> open · critical path: <K1> → <K2> → <K3>
| Ticket | Ready         | Role       | Edges           | Notes |
| <KEY>  | ✓ | gap: <…>  | crucial | deferrable | +by <K> −by <K> | <…>   |
Coverage: <parts with no ticket · tickets outside the plan · overlaps>
Plan: <the revised sequencing: the critical path, then the parallel hardening stream>
```

Then apply what the operator approves, all of it or the rows they pick, and say what you ran:

- **Edges**: `fleetc ticket block <KEY> --by <KEY>`, `fleetc ticket unblock <KEY> --by <KEY>`.
- **A ticket's gaps**: the project's grooming skill on it (`/<groom_skill> <KEY>`), else
  `fleetc ticket edit <KEY> --body -`, starting from `fleetc ticket <KEY>` (it replaces the
  description whole). Then weigh it back if it was anchored as not Ready: `fleetc weigh <KEY>`.
- **A missing ticket**: `/fleet-commission`, into this convoy.
- **The plan**: `fleetc convoy open <C> --plan -`, with the whole revised plan on stdin (its title
  stays). The plan's history keeps the old one (`fleetc convoy plan <C> --history`). Its Sequence
  names the path on one line, `- **Critical path:** <K1> → <K2> → <K3>`, the proof last:
  `/fleet-convoy-report` reads it.
