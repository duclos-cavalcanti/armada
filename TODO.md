# Armada TODO

- [ ] **A fleetc write never waits behind a sync.** fleetd runs every write on its one loop thread,
  the same thread that runs the sync, reconcile and health (reads are answered off it, on the
  server's threads). The 5-minute sync holds that loop while it reads the tracker and the forge
  (100–110 s on the live project before its per-ticket Jira reads were batched; measure it again),
  so a write landing in it waits it out. Time each step of `tick_sync`, then fetch off the loop and
  apply on it: the network calls on a worker thread, the ledger writes on the loop.

- [ ] **Make /fleet-ready-done project-agnostic again.** The splits in Ready #4 (new from routine,
  contracts in steps, decision docs before the code) and the ruling-added AC in Done #5 came from
  one project's MRs and assume its kind of work: deployed workflows and design docs. Keep the kit's
  floor generic and move what is one project's into that project's own doctrine.

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
  frontmatter (it fires under `claude --agent`) that exits 2 on those verbs. fleetd takes the
  client's word as well: `fleetc ruling` sends `delegated` from `FLEET_DELEGATION` and fleetd
  records the ruling as the admiral's on it, and `fleetc inspection` refuses only a caller with
  `FLEET_TICKET` or `FLEET_SHIP` set.
- [ ] **`fleetc transcript --text [N]`.** Print a cog's last N turns as plain text (the user and
  assistant messages, tool calls named), instead of the path. Each lookout in /fleet-inspect
  reinvents the extraction from the raw JSONL (`tail -c`, `grep -o '"text"'`, `jq`, Python),
  and a byte tail cuts the cause off. One source of truth for reading a cog's reasoning.
- [ ] **A CI retry budget per head.** /fleet-repair retries a transient failure once per sailor,
  but each sailor starts fresh, so a flaky job is retried without end: one MR's sailors re-ran the
  same flaky timing test 125 times in 6 hours. After 2 failed runs of the same job on the same
  head, stop retrying: propose the flake fix (/fleet-propose) and sleep. Count the runs from the
  forge (the job's runs on that head), so the budget holds across sailors.
- [ ] **A cheaper re-review.** A re-review now judges only the change since the head its last
  review covered (`FLEET_REVIEWED_HEAD`). Measure cost per review for a day, then try Sonnet
  and/or `effort: medium` on re-reviews, Opus on the first.
- [ ] **Keep factory ids out of team-facing artifacts.** Escalation case numbers (`case #77`),
  convoy numbers (`convoy #2`) and fleet roles leak into design docs, Jira bodies and MR replies,
  and the team can't map them to anything. A merged Feature Design freezes with them: one design
  carried 11 before merge, and another merged with one. Enforce it at three points:
  - **Record a team anchor per case.** Have `fleetc ruling` take one (the MR thread and the date)
    and send it to the cog with the ruling.
  - **Tell the cogs.** The worker, sailor and admiral prompts and /fleet-parley say to cite a
    ruling by its anchor and substance, never by case id.
  - **Guard the outputs.** `fleetc ticket new|edit|comment`, `fleetc reply` and `fleetc launch`
    refuse a body matching `case #\d+` or `convoy #\d+`, and /fleet-review flags them in a diff
    as a should-fix.

  The ledger, plans and case records keep the ids: they stay inside the factory.
- [ ] **Record a ship's lifecycle as events.** `ShipOpened`, `ShipSynced`, `ShipMerged` and
  `ShipClosed` are event kinds nothing writes, so a merge leaves no history: counting merges per
  day reads `ships.ended_at` against the snapshot's `merged`. Append `ShipOpened` when a ship is
  recorded and `ShipMerged` / `ShipClosed` in the same txn that ends it (payload: iid, ticket,
  pilot); drop `ShipSynced` unless a reader wants it (snapshots already hold the latest). Then the
  cost and convoy reports, and the dashboard, can count merges from events.
- [ ] **A test that fails when an import points outward.** The layering (code points inward to
  `domain`, logic reaches the outside only through its ports) is held by review alone. Add a
  `#[test]`, so `cargo test` gates it, at two levels:
  - **Crates.** `domain` and `wire` depend on no workspace crate, `ledger` only on `domain`, and
    nothing depends on `fleetd`, `fleetc` or an app (read from each `Cargo.toml`).
  - **fleetd's modules.** The logic (`reconcile`, `sync`, `health`, `crossings`) never names an
    adapter (`gitlab`, `github`, `jira`, `filesystem`, `tmux`, `git`, `server`) outside
    `#[cfg(test)]`; only the wiring (`main`, `scheduler`) builds them.

  It fails today: `reconcile.rs` builds `crate::git::GitWorktrees` itself three times (berth, cut
  and strike a review worktree), and `domain::ports` has no worktree port. Add the port first.
- [ ] **A held ticket still gets a builder.** Reconcile's spawn gate takes a hand order whatever
  its ticket, and a stacked worker through `stack_base`; only `is_ready` checks `held`. An anchored
  ticket dispatched to a hand, or stacked, gets a cog. Gate both on the hold.
- [ ] **Anchor asks a captain or a lookout for a state it has no word for.** `ANCHOR_MSG` tells
  every cog to report `anchored`; `CaptainState` and `LookoutState` have none, so the report is
  refused. Their holds live on the convoy and the `lookout` row, never the session, so their
  stand-downs' `anchored_at` guards never apply. Send them their own message (finish this pass,
  then stop) and drop the dead guards. (A ticket's anchor skips its admiral.)
- [ ] **The 100-item caps are silent.**
  - Jira's `board()` and `children()` read jira-cli's first page only (it ignores the offset). A
    sprint or an epic past 100 is cut short unannounced: a convoy misses tickets, and
    `roll_up_parent` could close a parent off its first 100 children. Page the REST search
    (`/rest/api/3/search/jql`, `nextPageToken`), or warn on a full page.
  - GitHub's armada is `pr list --limit 100`. Past 100 open PRs, `retire_departed` re-reads ours,
    finds them open, and retires them anyway: it spares only a merged one. Page the list, and never
    retire a ship its re-read shows open.
- [ ] **A report that skips a step isn't refused.** `record_status` gates `trials`, `concluded` and
  a hand's `done`; none checks where the cog comes from. `waiting` means reviewed clean, yet an
  unreviewed worker can report it and sleep until its blockers merge. Gate it as `concluded` is
  (reviewed at its head, no open thread), then decide which other edges earn a gate.
- [ ] **Docs and contracts out of date.**
  - `max_rulings` counts the admiral's rulings too (`rulings_since_weigh` counts every ruled case),
    while its config doc and `Capped` say the operator's. Decide which is meant; align the other.
  - `admiral.md` dispatches with no `--worktree`, which spawn refuses ("order has no
    worktree_path"), and the admiral has no `/fleet-berth`. Give it both.
  - `schema.sql` says a new proposal's ticket is filed on accept; `propose()` files it at once.
  - `/fleet-inspect`'s description names a hand's escalation; `HandState` has no `escalated`.
  - The filesystem tracker's `children()` matches `parent:`, but its epic is `convoy:` (what
    `set_epic` writes), so `children(<epic>)` finds none of a convoy's tickets.
  - Dead words: `WorkerState::Started` and `HandState::Awaiting` / `Implementing` parse but nothing
    reports or reads them. fleetd's `main.rs` header still calls the poll loop and `/context` "the
    next increment".
- [ ] **See a merge train drop an MR.** A train that can't rebase an MR removes it and disarms it,
  but sets no `mergeError` and leaves it `mergeable`, so no sailor comes (one MR sat
  dropped three times). Read GitLab's reason: the MR's system note "removed this merge request from
  the merge train because … failed to rebase", newer than its last arm ("enabled automatic add to
  merge train") and its last push, makes it `need_rebase`; `/fleet-rechart` then tries the server
  rebase, and a failed one reads `conflict` (local). The merge-train API can't: it 404s an MR off
  the train. One notes read per unarmed MR of ours each sync, and it parses GitLab's wording. Rare,
  so it waits; the cheaper fallback is to record the head `fleetc arm` armed and flag an MR
  disarmed at it.
