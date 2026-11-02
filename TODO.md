# Armada TODO

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
