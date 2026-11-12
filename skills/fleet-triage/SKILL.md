---
description: >-
  The operator's day-to-day sweep of a running fleet: settle the open proposals, re-judge the
  anchored tickets, keep every ticket in the convoy it belongs to, and check the fleet's pace
  against its deadlines and its machinery's health. Recommends, one numbered action each with its
  exact fleetc command, and acts only on the operator's OK. For the shipwright.
argument-hint: "[<convoy KEY|#N>] (default: every active convoy)"
---

# Skill: fleet-triage

You sweep what waits on the operator and what threatens the fleet's deadlines, and bring back a
short list of decisions, each ready to run. You change nothing until the operator says which to
apply. Grooming a whole convoy is the admiral's (`/fleet-groom-convoy`); ruling on an escalation is
the operator's.

## Step 0 — Ground

```bash
fleetc convoy list                 # the active convoys (#N), each with its open tickets
fleetc convoy plan <C>             # what each convoy is for: the measure of where a ticket belongs
fleetc proposal                    # the open proposals; one in full: fleetc proposal <id>
fleetc ledger "SELECT key, status, convoy FROM tickets WHERE held = 1 AND status NOT IN ('done','cancelled')
               AND key NOT IN (SELECT ticket FROM orders)"   # the anchored tickets; each one's why: fleetc ticket <KEY>
fleetc context --convoy <C> --format table   # each convoy's tickets: dispatched, cogs, ships, held
fleetc escalations --key <KEY>     # a convoy's open cases
fleetc plan                        # fleetd's last pass: what it planned, applied, failed
```

And the deadlines from the tracker: the sprint's end, and each ticket's due date where it has one
(Jira: `jira sprint list --state active`, a ticket's `duedate`).

## Step 1 — Sweep

- **Proposals.** Each one: accept it into the convoy whose plan it serves (`--convoy`), drop it
  (a duplicate, obsolete, or out of every plan; cite which), or leave it for the admiral when it
  needs real grooming first. One whose why starts "Amend:" asks an edit to that ticket, written
  in its comments: apply it (`fleetc ticket edit`, or `rework` if its MR is open), then drop the
  proposal, unless the ticket should also join a convoy.
- **Anchored tickets.** Re-judge each hold against its reason: the ruling it waited on is made, the
  blocker landed, the ticket has gone Done or Cancelled elsewhere. Then weigh it, leave it,
  `recall` it to the backlog, or `decommission` it. A hold that only waits on a blocker its edge
  names is redundant: weigh it, or, dispatched with its cog at the wall, `recall --weigh` it. A hold
  a convoy anchor placed ("convoy <C> anchored") goes with the convoy, never alone.
- **Placement.** A ticket whose work another convoy's plan owns moves there (`convoy remove` /
  `add`); a ticket in no convoy that one needs joins it; fleet-wide work no plan owns (a CI fix)
  joins the standing convoy; a convoy with nothing left open closes, unless it's standing.
- **Pace.** For each convoy, its critical path (the `*` tickets in `fleetc context --convoy <C>
  --format table`) against the sprint's end and the due dates: what's stuck on it (anchored,
  escalated for long, a dead cog, waiting on a blocker that isn't moving, in another convoy too),
  what won't land in time, and the one action that unblocks it. A convoy with no goal has no path:
  its grooming names one (`/fleet-groom-convoy`, the admiral's).
- **Machinery.** fleetd up, its health light, dry-run on or off as the operator intends, and the
  last plan's failures. A fault in the kit itself is yours to fix, as any kit bug.

## Step 2 — Report, then act on the OK

Report, nothing written yet:

```
Triage · <project> · sprint ends <date> (<n> days)
Proposals   <id> <KEY> → accept into #N | drop (<why>) | admiral (<why>)
Anchored    <KEY> → weigh | leave | recall | decommission — <why>
Placement   <KEY> → #N (<why>)
At risk     <KEY> (#N) — <what's stuck> → <the unblock>
Machinery   <what's off>, if anything
Actions
  1. <exact fleetc command>
  2. …
```

Then run the numbered actions the operator approves, all or the ones they name, and say what ran.
