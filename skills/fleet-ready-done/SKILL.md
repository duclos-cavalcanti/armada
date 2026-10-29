---
description: >-
  The fleet's Definition of Ready and Done — how a ticket is structured, when it is
  ready to dispatch (Ready), and how an MR meets it (Done). Reference, not an action:
  /fleet-commission and /fleet-propose draft tickets to it, the captain judges Ready against it before
  dispatch, the worker and the reviewer judge Done against it before a ticket is
  concluded. The kit's floor: a project's own stricter doctrine (in its repo) extends it.
---

# Ready and Done

The kit's floor for a ticket and the MR that delivers it. A project may hold its own,
stricter Definition of Ready / Done in its repo (e.g. `docs/`): where it is stricter, it
wins. This covers *what* a good ticket and a finished MR are, not the process around them
(dispatch, handoff, merge timing are the fleet's and the project's).

## The ticket

- **Title** — one action on a named thing: `Add neg to the sandbox crate`, `Replace the
  socket read with a full-body read`. It names the thing, never another ticket's ID, and
  survives a quarter (`Phase 4c` doesn't).
- **Type** — `story` (user-visible change), `bug` (defect), `task` (engineering work),
  `epic` (a convoy: groups tickets).
- **Body** — four sections:

```markdown
## Outcome
<1–2 sentences: the visible change once this ships.>

## Why
<The motivation: what problem this solves, what it unblocks.>

## Acceptance criteria
- <Each line testable: a reviewer can say yes or no to it.>

## Out of scope
- <What this deliberately does not do, and why — so nobody widens it silently.>
```

- **Links** — every dependency is an "is blocked by" edge, never prose ("after X lands").
  An epic is its convoy; a ticket from another epic it depends on (one it can't re-parent) joins
  with `fleetc convoy add <EPIC> <KEY>` — a ticket is in one convoy.

## The convoy plan

A convoy (`fleetc convoy open <KEY> --plan <file|->`) may carry a plan: what ties its tickets
together, for the captain that dispatches them. Kept short:

```markdown
## Outcome
<What the convoy delivers once every ticket lands.>

## Sequence
<The order its tickets land in, and why. The "is blocked by" edges enforce it; this explains it.>

## Invariants
- <A rule every ticket in it keeps.>

## Rulings
- <date — the decision, who made it, the tickets it binds.>
```

## Drafting a ticket

How `/fleet-commission` (files it) and `/fleet-propose` (a cog files it as a proposal for the operator)
both write one:

1. **Draft** the title and the four-section body above, from what you know (ask only for
   what's missing).
2. **Check it's new** — search the board for the title's key terms (filesystem tracker:
   `grep -ril "<term>" <tickets folder>`; Jira: its search). If an existing ticket already
   covers it, stop and name that ticket instead (a cog proposes taking it on: `/fleet-propose`).
3. **Wire it** — each existing ticket it needs first is a `--blocked-by <KEY>`; the epic it
   belongs to, if any, is `--convoy <KEY>`.
4. **Check Ready** (below) — a gap (an untestable criterion, two goals, too big for one MR) is
   fixed now: reshape it, or split it into several tickets with their order as blockers.
   Never write a ticket you'd have to hold, or one built on guesswork.

## Ready — before a ticket is dispatched

1. **Goal clear** — one action on a named thing; not two outcomes joined by "and also";
   the body says more than the title.
2. **Acceptance criteria testable** — each one checkable, none vague ("works well").
3. **Dependencies discharged** — blockers are edges; an open blocker means wait (fleetd
   holds the ticket off the frontier on its own).
4. **Sized to one MR** — if it needs several independent changes, split it: each piece
   Ready on its own, their order wired as "is blocked by" edges.
5. **Out of scope stated** — the edges of the work are written down.

A ticket that fails one of these isn't built on guesswork: it's fixed, or anchored with the
reason (`fleetc anchor <KEY> --reason "<what's missing>"`).

## Done — before a ticket is concluded

1. **Every acceptance criterion met, each proven** — by a test, or by a verification
   stated in the MR. Believed-done isn't done.
2. **Tests ship in the same MR** as the change they cover.
3. **Review reaches nits only** — every finding above a nit is fixed or answered. For a
   worker, that's the `surveyor`'s verdict: an independent review in a fresh context.
4. **CI green** on the MR's head.
5. **Scope held** — the MR does what the ticket says. Anything found along the way that
   isn't in it becomes its own ticket (a cog proposes it with `/fleet-propose`), not a note.
