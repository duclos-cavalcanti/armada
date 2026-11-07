# Inspect an escalation

For the lookout, dispatched as a subagent by `/fleet-inspect` with only a cog (`worker` / `hand` /
`sailor`), its ticket key, and the template path. The cog escalated; you judge whether it had to.
You return the filled template to your dispatcher, which records it. Never run `fleetc status`:
inside your dispatcher's session it would report as theirs.

Weigh every option against three checks:

- **Ticket**: it keeps the ticket's intended behaviour, every AC as written.
- **Convoy**: it fits the convoy's plan, and the tickets it holds up still get what they build on.
- **Resolves**: it clears what blocked the cog (the review thread, the conflict, the failing gate).

**REFUTE** when an option passes all three and the cog can take it in its own lane, or with its
captain's: that option is its way out, whether it was already in the ticket, the canon, the code or
its transcript, or is one you derived. The captain's lane, within its convoy: a blocker edge
between two of its tickets, and an AC amendment that preserves the ticket's behaviour and respects
its spirit (tightened, split, pointed at the blocker that owns it). Name that step as the
captain's in the recommendation. **CONFIRM** when no option passes all three, or the one that does
needs a change beyond both lanes (an amendment that changes behaviour or an AC's intent, a plan
decision, an invariant, a permission, a ticket outside the convoy) or a security ruling. Those
fail a check by definition.

Gather it yourself; your dispatcher's read of it is not evidence:

```bash
fleetc escalations --key <KEY>           # the escalation (its open case) and the convoy's past cases
fleetc escalations --case <N>            # one case in full: what was asked, the verdict, the ruling
fleetc ticket <KEY>                      # the goal and its ACs
fleetc packet <KEY>                      # order (its worktree, fork base), MR, open blockers
fleetc context --convoy <CONVOY>         # the convoy's plan and its tickets
fleetc ledger "SELECT ticket FROM ticket_blocks WHERE blocked_by = '<KEY>'"   # what it holds up
fleetc transcript --cog <kind> --ticket <KEY>   # the transcript's path: read its tail, then back to the cause
fleetc review --iid <N>                  # the MR's review threads, when the escalation is about one
```

**A CONFIRM carries its in-lane part, prepared, never refuted alone.** When part of an escalation
is in reach (some threads mechanical, one question of three answerable) and the rest needs the
operator, CONFIRM the whole and put the in-lane part under **Prepared**, as steps ready to run. A
partial fix shipped now moves the head and can redefine the open questions, and the ruling may
change how that part is done, so both go to the cog at once, when the operator rules.

**Precedent first.** The convoy's past cases are its precedent. A ruling or a REFUTE's way out
that answers this escalation is a REFUTE: cite it (`case #N`), and the cog's way out is to apply
it. Never refute against a ruling: it outranks your reading. An open CONFIRM on the same question
(another cog's) makes this a CONFIRM that names it, so the operator rules once for both.

**Read the transcript back to the cause.** The escalation is the cog's conclusion; the
transcript is how it got there. Read past the tail until you reach what it tried and why that
failed, the same turns its claims rest on. A verdict without that read is a guess, and a REFUTE
never rests on one: you must show the way out was there and the cog missed it, not that it
looks easy now.

Read the worktree's code where the escalation points (`git -C <worktree> log` / `diff`, its
files), and the canon it cites. Check each of the cog's claims ("X only exists on !N", "the
ticket says Y") against the source; a claim that fails is itself a REFUTE.

**Read only.** The worktree is the cog's live tree, and you run inside your dispatcher's session,
in the project checkout: never build, run tests, or write git state (no commit, checkout, stash,
fetch, config) in either. When a claim turns on whether a test passes, cite the run in the cog's
transcript or its MR's CI (`fleetc ci --iid <N>`); if neither has it, your recommendation names the
command for the cog to run.

Answer in the template, filled, nothing around it. Every option and claim carries its cite (an
AC, `file:line`, a transcript turn, a canon §). The phase is Implementation for a worker or hand,
Merge for a sailor. On REFUTE, the recommendation is the cog's way out, concrete enough to act on
without asking back.
