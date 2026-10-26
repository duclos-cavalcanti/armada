# Armada known issues

Accepted edge cases: understood, not fixed, each with its workaround.

- **A blocker added after the sailor armed the MR doesn't stop the merge.**
  *Scenario:* the sailor arms PROJ-2's MR (auto-merge) while CI runs. You then add
  "PROJ-2 is blocked by PROJ-9". fleetd's blocker gate stops new sailors, but the arm
  already lives on the forge, so PROJ-2 merges as soon as CI goes green. GitHub has
  nothing to stop it. GitLab stops it only once PROJ-9 has an open MR, because that's
  when reconcile declares the `/blocks` dependency.
  *Workaround:* a late dependency is the operator's or captain's call. Run
  `fleetc hold PROJ-2`: the hold disarms the MR and marks it draft. `fleetc unhold` it
  once PROJ-9 lands, and a fresh sailor re-sweeps it.
  *Fix if it bites:* reconcile holds an armed MR on its own when a blocker appears. (An arm
  *after* the blocker lands is already refused: `fleetc arm` checks for open blockers.)

- **The Jira "is blocked by" link direction is unverified.**
  *Scenario:* `fleetc ticket block KEY --by BY` (and `fleetc ticket new --blocked-by`) on a Jira
  tracker runs `jira issue link BY KEY Blocks`, assuming Jira's convention that the first issue
  blocks the second. If it's reversed, the edge lands backwards and fleetd waits on the wrong
  ticket.
  *Workaround:* after the first block on a Jira project, check the edge in Jira's UI ("KEY is
  blocked by BY") before relying on it.
  *Fix:* verify against a live Jira; if reversed, swap the two keys in `JiraTracker::block`.
