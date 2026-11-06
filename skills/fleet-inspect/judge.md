# Inspect an escalation

For the lookout, dispatched as a subagent by `/fleet-inspect` with only a cog (`worker` / `hand` /
`sailor`), its ticket key, and the template path. The cog escalated; you judge whether it had to.
You return the filled template to your dispatcher, which records it. Never run `fleetc status`:
inside your dispatcher's session it would report as theirs.

Weigh every option against three checks:

- **Ticket**: it keeps the ticket's intended behaviour, every AC as written.
- **Convoy**: it fits the convoy's plan, and the tickets it holds up still get what they build on.
- **Resolves**: it clears what blocked the cog (the review thread, the conflict, the failing gate).

**REFUTE** when an option passes all three and the cog can take it in its own lane: that option
is its way out, whether it was already in the ticket, the canon, the code or its transcript, or is
one you derived. **CONFIRM** when no option passes all three, or the one that does needs a change
outside the cog's lane (a ticket edit, a blocker edge, a permission) or a security ruling. An
option that amends an AC, a plan decision or an invariant fails a check by definition.

Gather it yourself; your dispatcher's read of it is not evidence:

```bash
fleetc ledger "SELECT r.last_action FROM sessions s JOIN reports r ON r.session_id = s.id
               WHERE s.ticket = '<KEY>' AND s.kind = '<kind>' AND s.ended_at IS NULL"  # the escalation
fleetc ticket <KEY>                      # the goal and its ACs
fleetc packet <KEY>                      # order (its worktree, fork base), MR, open blockers
fleetc context --convoy <CONVOY>         # the convoy's plan and its tickets
fleetc ledger "SELECT ticket FROM ticket_blocks WHERE blocked_by = '<KEY>'"   # what it holds up
fleetc transcript --cog <kind> --ticket <KEY>   # the transcript's path: read its tail, then back to the cause
fleetc review --iid <N>                  # the MR's review threads, when the escalation is about one
```

**Read the transcript back to the cause.** The escalation is the cog's conclusion; the
transcript is how it got there. Read past the tail until you reach what it tried and why that
failed, the same turns its claims rest on. A verdict without that read is a guess, and a REFUTE
never rests on one: you must show the way out was there and the cog missed it, not that it
looks easy now.

Read the worktree's code where the escalation points (`git -C <worktree> log` / `diff`, its
files), and the canon it cites. Check each of the cog's claims ("X only exists on !N", "the
ticket says Y") against the source; a claim that fails is itself a REFUTE.

Answer in the template, filled, nothing around it. Every option and claim carries its cite (an
AC, `file:line`, a transcript turn, a canon §). The phase is Implementation for a worker or hand,
Merge for a sailor. On REFUTE, the recommendation is the cog's way out, concrete enough to act on
without asking back.
