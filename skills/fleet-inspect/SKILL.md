---
description: Inspect a cog's escalation before it reaches the operator. Dispatches the surveyor to weigh the escalated worker / hand / sailor's reason against its transcript, its worktree, its ticket and its convoy's plan, and to return a verdict in this skill's template — REFUTE (the answer is already in the ticket, the canon or the code: it's sent to the cog to unblock itself) or CONFIRM (the operator needs eyes on it). For the admiral and captains.
argument-hint: <KEY-N> (a ticket whose cog is escalated)
---

# Skill: fleet-inspect

A cog escalated. Before the operator spends attention on it, have the **surveyor** judge it in a
fresh context: is the escalation real, or is its answer already within reach? Either way the
operator gets one structured summary — [`verdict.md`](verdict.md), in this skill's folder.

## Step 0 — Find the escalation

```bash
fleetc ledger "SELECT s.kind, r.state, r.last_action FROM sessions s JOIN reports r ON r.session_id = s.id
               WHERE s.ticket = '<KEY>' AND s.ended_at IS NULL"
```

Inspect only a live worker or sailor whose state is `escalated` (a captain finds them flagged
`inspect` in `fleetc context`). None → say so and stop.

## Step 1 — Dispatch the surveyor

Dispatch the `surveyor` subagent (the `Agent` tool) with only this:

> Inspect the escalation of the `<kind>` on `<KEY>`. Answer in the template at
> `<this skill's folder>/verdict.md`.

Nothing else — not your read of it. It gathers the evidence itself, so its verdict is independent.

## Step 2 — Act on the verdict

Record it, whichever it is — the ledger keeps every verdict, and the dashboard shows it:

```bash
fleetc inspection <KEY> --verdict <confirm|refute> --body - <<'EOF'
<the verdict, as returned>
EOF
```

In a chat with the operator (the admiral), show them the verdict too. Then:

- **REFUTE** → send the cog its way out, so it unblocks itself:
  ```bash
  fleetc send --cog <kind> --ticket <KEY> "Inspected: REFUTE — <the recommendation, with its cites>. Unblock yourself and carry on."
  ```
- **CONFIRM** → send nothing; the escalation stands with the operator, and fleetd convenes an
  admiral on it (`admiral-<KEY>`) for them to rule with. When the recommendation
  names an action that is yours (a missing `is blocked by` edge, a ticket edit), propose it to
  the operator rather than taking it unasked.
