# Armada known issues

Accepted edge cases: understood, not fixed, each with its workaround.

- **A blocker added after the sailor armed the MR doesn't stop the merge.**
  *Scenario:* the sailor arms PROJ-2's MR (auto-merge) while CI runs. You then add
  "PROJ-2 is blocked by PROJ-9". fleetd's blocker gate stops new sailors, but the arm
  already lives on the forge, so PROJ-2 merges as soon as CI goes green. GitHub has
  nothing to stop it. GitLab stops it only once PROJ-9 has an open MR, because that's
  when reconcile declares the `/blocks` dependency.
  *Workaround:* a late dependency is the operator's or captain's call. Cancel the arm by
  hand: `gh pr merge --disable-auto <n>`, or "Cancel auto-merge" on the GitLab MR. A
  `fleetc hold` / anchor doesn't do it, because the arm lives on the forge, not in the fleet.
  *Fix if it bites:* a `Forge::disarm` that reconcile calls when a blocker appears on an
  armed MR.
