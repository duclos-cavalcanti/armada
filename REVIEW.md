# One reviewer: the lookout reviews every MR and judges escalations

Design doc. Status: built, not yet committed or run live.

Where the build differs from the plan below:
- **`fleetc review-post --thread`** opens a finding as a resolvable thread (GitLab discussions).
  GitHub has no top-level threads, so it refuses there for now.
- **The dashboard's `enter` is unchanged.** The handler's keys are fixed, never smart; a review's
  lookout is reached from the Reviews pane, which now shows what each lookout is on.
- **A hand's review on request** dispatches the lookout as a subagent on its branch (`/fleet-review`
  without an MR, posting nothing): the surveyor it used is gone.
- **The `waiting` wake** now says "push, then report review": the new head is reviewed before the
  worker concludes.

## Summary

- **The surveyor is removed.** Its review rubric becomes the `/fleet-review` skill. Its inspection
  rules move into `/fleet-inspect`.
- **The lookout reviews every MR, ours and allies'.** fleetd spawns it as a top-level cog. It runs
  the project's review skill (`review_skill` in `fleet.toml`), or `/fleet-review` when none is
  set.
- **The worker no longer reviews its own work.** It pushes, reports `review` and stops. A lookout
  reviews its MR and posts threads. fleetd wakes the worker to answer them, and accepts
  `concluded` only once a review of the current head comes back clean.
- **The lookout judges escalations as a subagent, and the captain acts on the verdict.** The
  captain dispatches it through `/fleet-inspect`, just as it dispatches the surveyor today.

## Why

- **No self-review.** Today the worker controls what the surveyor is told, how its verdict is read
  and when to stop. With a project skill such as `/mr-review`, the agent that runs the skill sorts
  the findings and settles the verdict. If the worker ran it, the author would be judging its own
  work.
- **Symmetry.** Our MRs and allies' MRs get the same review, under the same rubric, from the same
  cog.
- **The review becomes a project setting.** A project with its own review process names it once,
  and every review runs it.
- **fleetd enforces the review.** A worker can't conclude without a clean review of its current
  head.
- **The crossing check runs on our own MRs too,** so it catches our own convoys colliding with
  each other.

## Roles after

| Who | Runs as | Job |
|---|---|---|
| lookout | cog, spawned by fleetd | reviews an MR, ours or an ally's, with the project's review skill and the crossing check |
| lookout | subagent of the captain or admiral | judges an escalation (`/fleet-inspect`) and returns REFUTE or CONFIRM |
| captain | cog | dispatches the frontier, triages escalations (through the lookout) and acts on the verdicts, closes the convoy |
| worker | cog | implements; answers its review threads; concludes |
| surveyor | — | removed |

## Flows

### Our MR (new)

1. The worker implements, commits and pushes to its draft MR, then runs
   `fleetc status --cog worker --state review` and stops. Its `/goal` ends, and Claude sits idle
   in its pane.
2. **fleetd spawns the lookout.** On the next reconcile, a worker in `Review` whose MR head isn't
   reviewed, with no lookout on that MR, is due one. fleetd spawns the lookout in the worker's
   worktree with `FLEET_SHIP=<iid>`, and sets `ships.reviewed = head` (as `launch_lookout` does
   today).
3. **The lookout reviews and posts.** It runs `/<review_skill> <iid>`, or `/fleet-review`, and the
   crossing check. It posts must-fix and should-fix findings as threads on the MR. Nits and the
   verdict go in one summary comment. It never approves our MR. Then it reports `reviewed`, and
   fleetd stands it down.
4. **fleetd wakes the worker.** The trigger is a worker in `Review` whose head equals
   `ships.reviewed`, with no live lookout on the ship. The wake uses `host.send`, the same path as
   `fleetc send` and today's `Wake`, and records the worker as `implementing`.
   - **Threads open:** "review round N posted on !iid: answer the threads with /fleet-parley,
     push, report review."
   - **None open:** "review clean: conclude."
   - **Round cap reached:** "round N still has open threads: escalate with them."
5. **The worker answers and pushes.** The push moves the head off `ships.reviewed`, so the next
   report of `review` brings a fresh lookout.
6. **The conclude gate.** fleetd refuses `concluded` unless `ships.reviewed == head` and the
   snapshot shows no open threads. This sits next to the existing open-blocker check.

### An ally's MR (unchanged, apart from the rubric)

The lookout is spawned in the checkout when an ally's MR has code it hasn't reviewed. It runs the
project's review skill or `/fleet-review`, plus the crossing check. It posts, approves when the
review is clean and nothing crosses our work, and reports `reviewed`.

### An escalation (triage)

The captain runs `/fleet-inspect <KEY>`, which dispatches `Agent(lookout)` with an inspection
order: the cog's escalation, ticket, convoy plan, transcript and worktree. The lookout weighs it
against the three checks (ticket, convoy, resolves) and returns its verdict. The captain records
it with `fleetc inspect` and acts within its lane. A CONFIRM convenes the admiral, as today.
Reconcile's early wake of the captain on an escalation is unchanged.

## fleetd

- **`Review` worker state.** It's added to `is_paused`, so the worker is neither healed nor reaped
  while it waits.
- **Spawn trigger.** A worker in `Review` whose ship's head isn't equal to `ships.reviewed`, and no
  live lookout on the ship. Our MR is a draft, so this trigger keys on the worker's state, not on
  the ally trigger's `ShipState::Open`.
- **Working directory.** The worker's worktree, from the order's `worktree_path`. An MR that edits
  the review skill is reviewed with its own edits, which is accepted.
- **Wake.** A new reconcile action alongside `Wake`. It carries the round number and the
  open-thread count.
- **Conclude gate.** In `scheduler.rs`'s status handling: the reviewed head and zero open threads.
- **Round count.** The number of lookout sessions on the ship (`sessions.ship`). `ships.rounds`
  already counts sailor sweeps, so it isn't reused. Nothing changes in the schema.
- **`review_skill`, `review_rounds`, `self_lookouts`.** New project settings in `config.rs`.
  `self_lookouts` caps the lookouts on our own MRs, separately from `lookouts`, and defaults to
  uncapped. `review_skill` is passed to the lookout in its environment.
- **A lookout that dies before reporting `reviewed`.** Standing it down clears `ships.reviewed`, so
  that head is reviewed again.
- **Approval guard.** `fleetc review-post --approve` is refused on our own ships.

## Agents and skills

- **`skills/fleet-review/SKILL.md`** (new): the surveyor's rubric. Findings graded must-fix,
  should-fix or nit, each with its evidence, `also:` sites, and a candidate fix checked against
  every case. The verdict is `clean` or `changes`. Done is judged against `/fleet-ready-done` and
  the project's doctrine. It runs with or without a ticket.
- **`agents/lookout.md`**: picks its mode from its order:
  - **review**, as a cog: the `FLEET_SHIP` MR, ours or an ally's. It approves allies only.
  - **inspect**, as a subagent: an escalation it's handed. It returns the verdict and never runs
    `fleetc status`, because inside the captain that would report as the captain.

  On our MR, it checks that its worker is alive before reporting `reviewed`, and escalates if it
  isn't. It gets `effort: high` and adds `Skill` to its tools. Skills: `fleet-review`, `fleet-inspect`,
  `fleet-ready-done`. `skills:` only preloads; the project's skill is called by name at runtime.
- **`agents/worker.md`**: the survey loop becomes the `review` handoff, then answering threads
  when woken. It adds `fleet-parley` to its skills.
- **`agents/surveyor.md`**: deleted.
- **`skills/fleet-inspect/`**: dispatches `Agent(lookout)` in place of the surveyor, and takes over
  the inspection rules (the three checks, REFUTE or CONFIRM).
- **`agents/captain.md`, `agents/admiral.md`**: `Agent(surveyor)` becomes `Agent(lookout)`, plus
  wording.
- **`agents/shipwright.md`, `skills/fleet-ready-done/`, `skills/fleet-parley/`**: wording (the
  lookout now judges Done; parley also serves the worker).
- **`skills/fleet-new-project/`**: asks for `review_skill`.

## Dashboard

- The Reviews pane lists every live lookout, with a column saying what it's on: an ally's MR or
  our ticket. `enter` switches to it.
- The Impl row shows `review` while the worker waits on its lookout.
- An inspection runs inside the captain, so it doesn't appear in the Reviews pane. The escalated
  cog's row shows `triage`, as today.

## Files

| File | Why |
|---|---|
| `crates/domain/src/lib.rs` | `WorkerState::Review`, added to `is_paused` |
| `crates/fleetd/src/config.rs` | `review_skill`, `review_rounds`, `self_lookouts` |
| `crates/fleetd/src/reconcile.rs` | spawn a lookout on our MR in the worker's worktree, under its own cap; the review wake; clear `reviewed` when a lookout dies unreported; drop the stale `/mr-review` reference |
| `crates/fleetd/src/scheduler.rs` | the conclude gate; the approval guard |
| `crates/fleetc/src/main.rs` | `review` as a worker state |
| `skills/fleet-review/SKILL.md` | the rubric, from the surveyor |
| `agents/lookout.md` | the review and inspect modes; runs the project's skill |
| `agents/worker.md` | the `review` handoff; answering threads |
| `agents/surveyor.md` | deleted |
| `skills/fleet-inspect/*` | dispatches the lookout; holds the inspection rules |
| `agents/captain.md`, `agents/admiral.md` | `Agent(lookout)` |
| `agents/shipwright.md`, `skills/fleet-ready-done/`, `skills/fleet-parley/` | wording |
| `skills/fleet-new-project/SKILL.md` | asks for `review_skill` |
| `apps/dashboard/src/source.rs`, `src/ui/mod.rs`, `src/main.rs`, `README.md` | the `review` state; what each lookout in the Reviews pane is on |

## Decisions

1. **Parley is unchanged.** The worker answers the lookout's threads with `/fleet-parley` as it
   stands.
2. **Self-review lookouts have their own cap.** A new setting, `self_lookouts`, defaults to `-1`
   (uncapped). Allies keep `lookouts`.
3. **A dead worker in `Review` escalates through the lookout.** Before reporting `reviewed`, the
   lookout checks that its worker is alive. If it isn't, the lookout reports
   `escalated` ("worker for <KEY> is dead; review posted on !iid") and holds for the operator.
   - A worker dies here on a machine restart, a crash, or a killed pane.
   - A restart kills the lookout too. So when fleetd stands down a lookout that never reported
     `reviewed`, it clears `ships.reviewed`. A fresh lookout is then spawned, finds the worker
     dead, and escalates. Without this, `ships.reviewed`, which is set at launch, would mark a
     head reviewed that no lookout finished reviewing. Allies get the same fix: today a lookout
     that dies mid-review leaves that ally head reviewed forever.
4. **Hands skip review.** A hand concludes when the operator says so, as today.
5. **The stale reference goes.** `launch_lookout`'s doc comment stops naming `/mr-review`.

## Later

- The `ESCALATIONS.md` powers (blocker edges, in-convoy AC amendments, partial verdicts,
  precedent) land on the captain as the one acting on the lookout's verdicts.
- Confirm that a top-level `claude --agent lookout` calls an unlisted project skill. The docs
  confirm it for subagents only.
