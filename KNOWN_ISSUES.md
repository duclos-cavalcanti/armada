# Armada known issues

Accepted edge cases: understood, not fixed, each with its workaround.

- **A blocker added after the sailor armed the MR doesn't stop the merge.**
  *Scenario:* the sailor arms PROJ-2's MR (auto-merge) while CI runs. You then add
  "PROJ-2 is blocked by PROJ-9". fleetd's blocker gate stops new sailors, but the arm
  already lives on the forge, so PROJ-2 merges as soon as CI goes green. GitHub has
  nothing to stop it. GitLab stops it only once PROJ-9 has an open MR, because that's
  when reconcile declares the `/blocks` dependency.
  *Workaround:* a late dependency is the operator's or captain's call. Run
  `fleetc hold PROJ-2`: the hold disarms the MR on either forge. `fleetc unhold` it
  once PROJ-9 lands, and a fresh sailor re-sweeps it.
  *Fix if it bites:* reconcile disarms an armed MR on its own when a blocker appears.

- **A hold during a live sailor's sweep can be re-armed.**
  *Scenario:* a sailor is mid-sweep on PROJ-2 when you run `fleetc hold PROJ-2`. The hold
  disarms the MR, then the sailor finishes its sweep and arms it again.
  *Workaround:* hold again once the sailor reports `sleeping` (the sweep is short).
  *Fix:* step 7 routes arming through `fleetc`, and the server refuses to arm a held ticket.
