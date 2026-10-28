# Armada known issues

Accepted edge cases: understood, not fixed, each with its workaround.

- **A blocker added after the sailor armed the MR doesn't stop the merge.**
  *Scenario:* the sailor arms PROJ-2's MR (auto-merge) while CI runs. You then add
  "PROJ-2 is blocked by PROJ-9". `fleetc arm` refuses new arms while blocked, but this
  arm already lives on the forge, so PROJ-2 merges as soon as CI goes green. GitHub has
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

- **A `fleetc` call holding a shell variable goes to auto mode's classifier.**
  *Scenario:* Claude Code matches an allow rule only against a command it can check before it
  runs, so `Bash(fleetc *)` approves `fleetc arm` but not `fleetc arm "$FLEET_TICKET"`. That one
  goes to the classifier, which has blocked a sailor's arm as an "Unrequested Commit in a
  Connected App". The same holds for a compound command: every part must match a rule.
  *Prevention:* a cog's own ticket, MR and convoy are filled in by fleetc from `FLEET_TICKET` /
  `FLEET_SHIP` / `FLEET_CONVOY` when left out, and the agents and skills write their `fleetc`
  writes as standalone, variable-free commands. Keep new prose to that shape.
  *Workaround:* for a cog already stuck, answer it in its pane ("go, arm it"): your message is the
  explicit intent the classifier accepts.
