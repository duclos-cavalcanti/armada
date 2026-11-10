# dashboard

A live TUI over the fleet. Reads **fleetd's ledger read-only**
(the project's `.fleet/ledger.db` — the project enclosing the cwd, or `-C <dir>`) and renders the fleet at a glance,
refreshing once a second. A **viewer, not an orchestrator** — it renders state and
binds keys to copy it out; it never mutates the fleet (that's `fleetc`). Built on
[ratatui](https://ratatui.rs) + [crossterm](https://crates.io/crates/crossterm) — a
single static binary, no runtime dependencies.

## Tabs

Cycle with `Tab` / `S-Tab`:

- **Fleet** — the line, top-down: the open convoys, what waits to enter the line, every order,
  then the builders and the ships being landed:
  - **Convoys** — the open convoys: the captain's liveness (or off), how many tickets are still
    open, and of its critical path (`Path`: its goal and all it waits on; `—` with no goal);
    `enter` switches to the captain. On every other pane, a ticket on its convoy's critical path
    is starred beside the convoy (`#2*`);
  - **Proposals** — what waits on you before it can enter the line: the open proposals (`enter`
    reads one, `a` accepts, `d` drops). `h`/`l` swaps in **Anchored**: tickets held before they were
    dispatched, a captain's not-Ready ones among them (`⚓ <reason>`; `t` opens one; fix it, then `w`
    weigh). An anchored ticket that has an order shows in Dispatch instead, `⚓` in its "Waiting on";
  - **Dispatch** — the workers' orders (auto-implemented), with their phase (waiting /
    implementation / merge) and what they wait on (anchored ⚓ / blocked ⛔); `h`/`l` swaps in the
    hands' orders (operator-driven);
  - **Implementation** — builders at work (workers + hands): state, liveness, MR + pipeline, last
    action. A worker in **review** has put its MR up for a lookout and waits for its review;
  - **Merge** — our ships being landed: the sailor's state (dimmed once it's stood down), whether
    an admiral sits on its escalation (`Adm`: `up`; `e` switches to it), pipeline, approvals, armed / on a merge train, open threads, what it's blocked on, merge status;
  - an escalation reads by who holds it: **flagged** (no captain has looked), **triage** (a
    captain is inspecting it, or sent its way out), **escalated** (confirmed: yours), **ruled**
    (your ruling went to the cog, `fleetc ruling`; it clears when the cog moves on). `v` reads
    the verdict;
  - **Reviews** — every lookout on watch: the MR, what it's on (our ticket in review, or `ally`),
    its author, the lookout's state; `enter` switches to the lookout, `m` opens the MR;
- **Ships** — every open MR fleetd tracks, ours + allies; the title counts the lookouts on watch,
  and the last column shows its review (`◎` a lookout is on it, `✓` reviewed at its head).
- **Board** — the sprint tickets, with the Convoys and Proposals panes again (the same panes as
  on Fleet).
- **Health** — fleetd's health check, check by check, grouped (Kit, Git, Project Skills, Project
  Worktrees, Project Memory): `✓` ok, `!` a warning (lights the dashboard only), `✗` a failure
  (holds the lookouts), `–` nothing to check; what each checks, faded, and why one isn't ok with
  its fix. Below, every change of state, newest first. The title says when it last ran.
- **Log** — fleetd's log (`.fleet/fleetd.log`), newest first, warnings yellow and errors red;
  the title says whether fleetd is up. The footer flags a down fleetd from every tab.

The **Health** tab's name is the health light: green while fleetd's last health check passed,
yellow with a warning, red on a failure, when the footer says why and no lookout launches until the
check passes. The tab strip's title is fleetd's mode as its last reconcile pass ran:
`dry_run=true` in red (it only plans), `dry_run=false` in green.

## Keys

| Key | Action |
|---|---|
| `q` / `Esc` | quit |
| `r` | refresh now |
| `Tab` / `S-Tab` | next / previous tab |
| `↑` / `↓`, `C-p` / `C-n` | previous / next pane |
| `1`–`5` | focus the Nth pane |
| `j` / `k` | move the cursor within the focused pane |
| `h` / `l` | swap a pane for its alternate: proposals ↔ anchored, and workers ↔ hands in Dispatch and in Implementation. The title peeks at the hidden one: `/ Hands (3)` |
| `space` | zoom the focused pane (toggle) |
| `y` / `Y` | yank the focused pane (every row, in view or not) / the page as drawn |
| `enter` | switch to the row's tmux session (its cog) |
| `m` | open the row's MR |
| `t` | open the row's ticket |
| `v` | read the verdict on the row's escalation (Implementation, Merge) |
| `e` | switch to the admiral fleetd convened on the row's confirmed escalation |
| any other | run `handler.sh <tab/pane> <key> <row fields…>` on the focused row |

The pane titles name no keys: `?` lists the keys that work everywhere, then each pane's.

The keys are fixed, never "smart": `enter` on a row whose session is down (or has none) opens
nothing else — it fails. A failed action shows in red in the footer for a few seconds.

## Layout

```
apps/dashboard/
├── Cargo.toml
├── handler.sh       # the action handler: (tab/pane, key) → action
└── src/
    ├── main.rs      # entry, event loop, key handling, tab + pane focus
    ├── action.rs    # non-navigation keys → handler.sh (the action handler)
    ├── source.rs    # Fleet::load() — reads the ledger read-only (+ fleetd's log) into the view model
    └── ui/mod.rs    # tab strip + per-tab panes + footer (pure render over App)
```

## Build & run

```bash
cargo run -p dashboard -- -C projects/<name>         # or run it from inside the project
```
