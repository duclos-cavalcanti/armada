# Assess a ticket for dispatch

For the lookout, dispatched as a subagent by a captain (or the admiral) with only a ticket key and
this file's path: is the ticket Ready to dispatch? You read; you write nothing to the tracker or the
code. Your dispatcher acts on your answer.

1. **The project's check.** When the project names its own readiness check (`FLEET_ASSESS_SKILL`,
   from `assess_skill` in its fleet.toml), run it on the ticket (`/<that skill> <KEY>`) and take
   its verdict. When it names none, judge the ticket yourself against `/fleet-ready-done`'s Ready.
2. **The kit's check: each AC can be met given what its blockers deliver.** For every open
   `is blocked by` edge (`fleetc ticket <KEY>` lists them), read the blocker (`fleetc ticket
   <BLOCKER>`). An AC that needs something no blocker's ACs deliver, or that contradicts one, is an
   `AC-GAP`. So is an AC the real environment can't meet as the canon describes it (a credential the
   fleet can't hold, an environment that doesn't exist).

Answer with this, nothing around it:

```
VERDICT: READY | BLOCKED | UNCLEAR | INVARIANT-GAP | OVERSIZED | AC-GAP
<each finding, one line: what, and its cite (an AC, a blocker's AC, a canon §)>
```

The worst finding sets the verdict; READY carries none. Without a project check, use the same
words: `UNCLEAR` for a goal or AC too vague to build, `BLOCKED` for a blocker that isn't discharged.
