# Armada TODO

- [ ] **Let the captain resolve more escalations.** It acts on the lookout's verdicts, but a
  verdict is binary and every ruling starts from scratch. Give it the powers that keep escalations
  off the operator: add a missing blocker edge, amend an AC within its own convoy, and act on a
  partial verdict. (Past rulings are precedent already: `fleetc escalations`.)
- [ ] **Keep a cog to its own convoy's cases in `fleetc ledger` too.** `fleetc escalations` refuses
  a key outside the cog's convoy, but `fleetc ledger "SELECT * FROM cases"` reads every convoy's.
  Have `fleetc ledger` refuse a cog's query that names `cases` or `inspections`, so the only way in
  is the scoped verb. A guard against misuse, like the other cog checks, not a boundary.
- [ ] **Confirm a lookout cog can call the project's review skill.** `review_skill` names a skill
  that isn't in the lookout's `skills:`; the docs say a subagent can call an unlisted project skill
  by name, but not whether a top-level `claude --agent lookout` session can.
- [ ] **Reviving a sailor shouldn't go through `sink`.** Today an escalated sailor holds its slot
  until `fleetc sink` ends it; the sweep trigger then launches a fresh one. Scuttling as the way to
  revive muddies the verb. Decide the proper path: resume an escalated sailor when its ship's
  fingerprint changes since it escalated, and/or make `fleetc weigh` resume a dead cog (today
  weigh on a dead session does nothing, and its stale `anchored` report keeps it paused).
- [ ] **Spawned cogs stall on Claude Code's folder-trust prompt.** A cog launched in a folder
  Claude Code hasn't seen (a new worktree, or one that moved) opens on "trust this folder?" with
  "No, exit" selected, and no one is there to answer it. Find why (the checkout's
  `.claude/settings.json` adds a directory + permissions) and make spawns run unattended.
- [ ] **`sync_blocks` keeps stale open MR dependencies.** It drops only dependencies on closed
  or merged MRs, so a block whose ticket edge was removed stays while the blocker MR is open
  (an MR stayed blocked after its ticket lost the edge; removed by hand). Make
  it also drop open dependencies the ticket graph (`wanted_blocks`) no longer wants.
- [ ] **Hard-deny the operator's verbs to cogs.** `fleetc` refuses `decommission`, `rework` and
  `ticket new` inside a cog by its env (`FLEET_TICKET` / `FLEET_CONVOY` / `FLEET_SHIP`), which a
  cog could unset. Frontmatter `disallowedTools: Bash(...)` drops all of Bash, and a settings deny
  binds the admiral and shipwright too; the per-agent way is a `PreToolUse` hook in each cog's
  frontmatter (it fires under `claude --agent`) that exits 2 on those verbs.
- [ ] **`fleetc transcript --text [N]`.** Print a cog's last N turns as plain text (the user and
  assistant messages, tool calls named), instead of the path. Each lookout in /fleet-inspect
  reinvents the extraction from the raw JSONL (`tail -c`, `grep -o '"text"'`, `jq`, Python),
  and a byte tail cuts the cause off. One source of truth for reading a cog's reasoning.
- [ ] **A convoy's critical path, built in.** `fleetc convoy open <C> --goal <KEY-N>…` names the
  convoy's goal tickets (the proof its outcome is delivered); fleetd derives the critical path: the
  goals and every convoy ticket they wait on, through the blocker edges it already syncs. A
  `convoy_goals` table, `ConvoyRepo::{set_goals, goals}`, the closure a pure `domain` fn,
  `ConvoyTicketView.critical` and `ConvoyView.goals` (so `fleetc context` and `convoy plan` carry
  it). /fleet-convoy-report drops its plan regex and blockers SQL for them; /fleet-groom-convoy sets
  the goal and flags a crucial ticket outside the path (a missing edge); /fleet-triage flags an
  open convoy with none. Required by doctrine, not by the machine: no goal, no path.
- [ ] **A re-review reviews the delta, not the whole MR.** Every new head on our MR gets a fresh
  lookout that re-reviews everything, so verdicts flip (clean, changes, clean) and an MR costs ~9
  full reviews. Hand the lookout `ship.reviewed` (the head last reviewed): a re-review judges the
  diff since it and whether the open threads are fixed, and raises nothing new on code a prior
  review passed (a correctness or security defect excepted). Measure cost per review for a day,
  then try the cheaper re-review: Sonnet and/or `effort: medium` on re-reviews, Opus on the first.
- [ ] **A structured refused list.** `projects/<p>/refused.md` is prose. Make it
  `refused.toml`, one `[[refused]]` per entry (`id`, `what`, `why`, `ruling`), a serde type. The
  health check parses it when it exists: a parse error, a duplicate id or an empty field fails it,
  which holds the lookouts, so a broken list never silently weakens a review. The lookout cites a
  match by its id. A `fleetc refused` verb waits for a second reader (workers, triage).
