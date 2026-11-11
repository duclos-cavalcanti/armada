# Armada known issues

Accepted edge cases: understood, not fixed, each with its workaround.

- **A blocker added after the sailor armed the MR doesn't stop the merge.**
  *Scenario:* the sailor arms PROJ-2's MR (auto-merge) while CI runs. You then add
  "PROJ-2 is blocked by PROJ-9". `fleetc arm` refuses new arms while blocked, but this
  arm already lives on the forge, so PROJ-2 merges as soon as CI goes green. GitHub has
  nothing to stop it. GitLab stops it only once PROJ-9 has an open MR, because that's
  when reconcile declares the `/blocks` dependency.
  *Workaround:* a late dependency is the operator's or captain's call. Run
  `fleetc anchor PROJ-2`: the anchor disarms the MR and marks it draft. `fleetc weigh` it
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
  *Fix:* verify against a live Jira; if reversed, swap the two keys in `JiraTracker::link`.
  `fleetc ticket unblock` reads the edge from the same side, so a reversed edge is one it can't
  find either: remove it in Jira's UI.

- **`fleetc recall`'s Jira steps are unverified.**
  *Scenario:* a recall returns the ticket with `jira issue move <KEY> "To Do"`, a REST unassign, and
  Jira Agile's `POST /rest/agile/1.0/backlog/issue` (out of the sprint). A workflow that doesn't allow
  To Do from where the ticket stands, or a board without the Agile API, fails that step.
  *Workaround:* the recall goes on (MR closed, cogs ended, worktree discarded, ticket anchored) and
  names the failed step; finish it in Jira by hand.
  *Fix:* verify against the live board on the first recall.

- **An admiral finds the project's grooming skill without `FLEET_GROOM_SKILL`.**
  *Scenario:* an admiral fleetd convenes gets `groom_skill` in `FLEET_GROOM_SKILL`. One opened
  another way (the dashboard's `a`, `claude --agent admiral`) has no such variable,
  but `admiral.md` names the source (`groom_skill` in fleet.toml), so it reads fleet.toml and runs the
  project's skill all the same (verified: it answers `/groom-ticket <KEY>` either way). The variable
  is a delivery, not an opt-in.
  *Workaround:* to groom with `/fleet-ready-done` alone, leave `groom_skill` out of fleet.toml.
  *Fix if it bites:* make the variable the only switch — `admiral.md` names `FLEET_GROOM_SKILL` and
  not fleet.toml — so only fleetd's admirals bring the project's skill in.

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

- **Clients read the ledger directly, not through fleetd.**
  *Scenario:* fleetd is the single writer, but reads bypass it. The dashboard builds its view from
  the ledger (`apps/dashboard/src/source.rs`, with `GET /context` only for liveness); `fleetc` opens
  it read-only for `convoy list|plan`, `inspection`, `escalations`, the recall/decommission preview
  and `#N` resolution, and reads the telemetry DB for `cost`; `/fleet-convoy-report` and
  `/fleet-cost-report` query both with SQL. The original plan had the TUIs read through fleetd over
  `wire` (read parity: "`crew`/`dashboard` read from `fleetd`"); the code went direct, and the kit's
  conventions now say "reads may be direct (read-only)".
  *Cost:* every reader is coupled to the storage schema, and the meaning on top of it is re-derived
  per reader: a ticket's open blockers lives in `scheduler::waiting_on`, `reconcile::open_blockers`
  and the dashboard's `open_blockers`. Facts only fleetd holds in memory (a cog's liveness) can't
  be read from the ledger at all, and only a Rust client on the same host can use the `ledger`
  crate.
  *Workaround:* none needed today; when changing a derivation, change every copy.
  *To decide:* whether fleetd serves the views it can derive (as `GET /context?convoy=` does for a captain:
  e.g. `GET /fleet` for the dashboard) and clients only render them, with direct ledger reads kept
  for debugging (`fleetc ledger`). Then: which reads move first, whether the TUI migrates, and what
  of the telemetry DB fleetd serves. Parked with the browser-dashboard idea, which depends on it.

- **`fleetc`'s `import_restores_a_snapshot_and_drops_the_stale_wal` test is flaky.**
  *Scenario:* it builds a throwaway project under the temp dir and binds `.fleet/fleetd.sock`
  there. Two failures seen: (1) with a long `TMPDIR` (a sandboxed agent's), the socket path
  passes macOS's 104-byte limit and the bind fails every time ("path must be shorter than
  SUN_LEN"); (2) in some `cargo test --workspace` runs, `import_into` errors intermittently.
  It passes when rerun or run alone (`cargo test -p fleetc`); (2)'s cause isn't pinned down.
  *Workaround:* rerun, or run with `TMPDIR=/tmp`.
  *Fix if it bites:* root the test project under a short path (e.g. `/tmp/fi-<pid>`), and print
  `import_into`'s error in the assertion so (2) names itself.

- **Only joining a convoy and dispatching check that a ticket is ours.**
  *Scenario:* the fleet takes on only a ticket that's yours or nobody's. A teammate holds it while
  it's assigned to them or, unassigned, while their MR is open on it. `convoy open` (over its
  epic's open tickets), `convoy add`, `proposal accept --convoy` and `dispatch` refuse their ticket
  (`NOT-OURS`), and the captain's frontier skips it. Two paths don't check:
  - A teammate who takes a ticket in an open convoy's epic later: it stays in the convoy (never
    dispatched, but a blocker we don't control).
  - The other verbs that write to a ticket run on a teammate's all the same: `decommission` closes
    their MR and cancels the ticket, `recall` closes it and unassigns them, `rework` puts a worker on
    it, `anchor` / `weigh` (`--convoy` too) draft and ready it, and `ticket edit` / `comment` /
    `block` / `unblock` / `relate` rewrite their ticket.
  *Workaround:* before acting on a ticket outside your convoys, check who holds it
  (`fleetc ticket <KEY>`, and its MRs). Triage's pace check names a convoy ticket that's stuck.
  *Fix if it bites:* put `own` (scheduler.rs) in front of each of those verbs, on the ticket acted
  on (not a `--by` / `--to` one), and have sync flag a convoy ticket a teammate takes.
