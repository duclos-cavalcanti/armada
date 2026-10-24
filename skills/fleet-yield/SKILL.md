---
description: Yield a hand's work to a sailor — the operator's "pawn it off" handoff. Reads the hand's ticket from `FLEET_TICKET` (fleetd injects it at spawn) and, if there is one, reports `concluded` via fleetc so fleetd hands the open MR to a sailor (promotes the order to auto, moves Jira → In QA, requests review, reaps the hand). A ticketless hand has no sailor to yield to. Run from the hand's worktree.
argument-hint: (none — reads $FLEET_TICKET)
---

# Skill: fleet-yield

You are a **hand** yielding your work to a **sailor**: the operator has said you're
done driving the ticket by hand, and you're handing the open MR off to be sailed to
merge. This is a thin wrapper around one `fleetc` call — the `concluded` report is
the handoff signal.

## Preconditions

- An **open MR** must already exist for your work — the sailor sails an *existing*
  ship; if nothing's pushed, there's nothing to yield.
- Run inside the hand's session, so `FLEET_TICKET` (injected by fleetd) is set.

## Step 0 — Derive the ticket + yield

Run in one Bash invocation (from your worktree):

```bash
TICKET="${FLEET_TICKET:-}"
if [ -z "$TICKET" ]; then
  echo "no FLEET_TICKET — a ticketless hand has no sailor to yield to; nothing to do."
  exit 0
fi
fleetc status --cog hand --ticket "$TICKET" --state concluded \
  --last-action "operator yielded the hand to a sailor"
echo "yielded $TICKET to a sailor."
```

That `concluded` report is the whole handoff: fleetd promotes your (manual) order to
auto, moves Jira In Progress → In QA, requests the team's review, reaps you, and
`LaunchSailor` engages a sailor **in this worktree** to sail the MR to merge. You do
**not** self-exit — the report retires you.

## Checklist

- [ ] An open MR exists (there's a ship for the sailor to sail)
- [ ] Ticket read from FLEET_TICKET; if unset, reported "nothing to yield" and stopped
- [ ] `fleetc status … --state concluded` sent (ticketed hand only)
