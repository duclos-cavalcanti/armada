---
description: Inspect a cog's escalation before it reaches the operator. Dispatches the lookout to weigh the escalated worker / hand / sailor's reason against its transcript, its worktree, its ticket and its convoy's plan, and to return a verdict in this skill's template — REFUTE (an option keeps the ticket's intent, fits the convoy's plan and clears the blocker: it's sent to the cog to unblock itself) or CONFIRM (no option passes all three: the operator rules). For the admiral and captains.
argument-hint: <KEY-N> (a ticket whose cog is escalated)
---

# Skill: fleet-inspect

A cog escalated. Before the operator spends attention on it, have the **lookout** judge it in a
fresh context, as a subagent: is the escalation real, or does a way out keep the ticket's intent, fit the
convoy's plan and clear the blocker? Either way the
operator gets one structured summary — [`verdict.md`](verdict.md), in this skill's folder.

## Step 0 — Find the escalation

```bash
fleetc ledger "SELECT s.kind, r.state, r.last_action FROM sessions s JOIN reports r ON r.session_id = s.id
               WHERE s.ticket = '<KEY>' AND s.ended_at IS NULL"
```

Inspect only a live worker or sailor whose state is `escalated` (a captain finds them flagged
`inspect` in `fleetc context`). None → say so and stop.

## Step 1 — Dispatch the lookout

Dispatch the `lookout` subagent (the `Agent` tool) with only this:

> Inspect the escalation of the `<kind>` on `<KEY>`, by `<this skill's folder>/judge.md`. Answer
> in the template at `<this skill's folder>/verdict.md`.

Nothing else — not your read of it. It gathers the evidence itself, so its verdict is independent.

## Step 2 — Act on the verdict

Record it, whichever it is — the ledger keeps every verdict, and the dashboard shows it:

```bash
fleetc inspection <KEY> --verdict <confirm|refute> --body - <<'EOF'
<the verdict, as returned>
EOF
```

In a chat with the operator (the admiral), show them the verdict too. Then:

- **REFUTE** → when the way out names a step of the captain's (a blocker edge, an AC amendment
  that keeps behaviour and spirit), take it first, in your convoy: `fleetc ticket block <KEY> --by
  <KEY>`, or `fleetc ticket edit <KEY> --body - --why "<…>" --case <N>` (fleetd records the diff on
  the ticket). Then send the cog its way out, so it unblocks itself:
  ```bash
  fleetc send --cog <kind> --ticket <KEY> "Inspected: REFUTE — <the recommendation, with its cites>. Unblock yourself and carry on."
  ```
- **CONFIRM** → send nothing; the escalation stands with the operator, and fleetd convenes an
  admiral on it (`admiral-<KEY>`) for them to rule with. Their ruling goes on the case
  (`fleetc ruling --case <N>`), which sends it to the cog and keeps it for the convoy's next one. A
  CONFIRM's recommendation is beyond your lane by definition (an amendment that changes behaviour,
  the plan, a ticket outside your convoy): propose it to the operator rather than taking it.
