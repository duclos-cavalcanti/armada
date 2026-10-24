# MADY fleet TODO

- [ ] **Cutover — seed the fresh ledger from the pre-cutover manifest.** *(the last step: go live)*
  *What:* the fleet boots on a clean ledger (rebuilt from Jira/glab sync). Re-inject the work in flight in the old factory so the fresh fleet resumes it. From the FLEET_PROGRESS cutover manifest (git `a935716~1`), translate the 7 captured cogs:
    - **sailors** (ex-finishers, open MRs): `fleetc dispatch --ticket MADY-6127|MADY-6183|MADY-6197 --worktree <preserved>` — reconcile's LaunchSailor fires on the synced open ship.
    - **hands** (ex-humans): `fleetc dispatch --ticket MADY-6164|MADY-5064 --hand --worktree <preserved>`.
    - **captains** (ex-foremen): `fleetc convoy open MADY-3098` + `fleetc convoy open MADY-4411` — reconcile staffs the captain per convoy.
  *Goal:* `make run` on a fresh ledger + this seed = the fleet picks up exactly the in-flight tickets/MRs in their preserved worktrees. Optionally reuse each cog's prior harness session id (in the manifest) via `claude --session-id` so a resumed cog continues its predecessor's transcript rather than starting cold.
  *Need:* without seeding, a fresh fleet starts empty and the in-flight work is orphaned. First verify the preserved worktrees still exist under `worktrees/` (the manifest was captured 2026-10-19).
