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
    open; `enter` switches to the captain;
  - **Proposals** — what waits on you before it can enter the line: open proposals (`a` accept,
    `d` drop) and convoy tickets a captain anchored as not Ready (`⚓ <reason>`; fix the ticket, then
    `w` weigh). `enter` reads a proposal, `t` opens an anchored ticket;
  - **Dispatch** — every order the fleet is working, with its phase (waiting / implementation /
    merge) and what it waits on (anchored ⚓ / blocked ⛔); `h`/`l` swaps in **Hand dispatch**, the
    hand-dispatched orders alone;
  - **Implementation** — builders at work (workers + hands): state, liveness, MR + pipeline, last
    action. A worker in **review** has put its MR up for a lookout and waits for its review;
  - **Merge** — our ships being landed: the sailor's state (dimmed once it's stood down),
    pipeline, approvals, armed / on a merge train, open threads, what it's blocked on, merge status;
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
- **Log** — fleetd's log (`.fleet/fleetd.log`), newest first, warnings yellow and errors red;
  the title says whether fleetd is up. The footer flags a down fleetd from every tab.

The tab strip's title is the health light, `fleet: ●`: green while fleetd's last health check
passed (the ref guard is linked, the checkout's git settings are clean). On a miss it goes out
(`○`, red), the footer says why, and no lookout launches until a check passes.

## Keys

| Key | Action |
|---|---|
| `q` / `Esc` | quit |
| `r` | refresh now |
| `Tab` / `S-Tab` | next / previous tab |
| `↑` / `↓`, `C-p` / `C-n` | previous / next pane |
| `1`–`5` | focus the Nth pane |
| `j` / `k` | move the cursor within the focused pane |
| `h` / `l` | swap a pane for its alternate: Dispatch ↔ hand dispatch, Implementation's workers ↔ hands. The title peeks at the hidden one: `/ Hands (3)` |
| `space` | zoom the focused pane (toggle) |
| `y` / `Y` | yank the focused pane (every row, in view or not) / the page as drawn |
| `enter` | switch to the row's tmux session (its cog) |
| `m` | open the row's MR |
| `t` | open the row's ticket |
| `v` | read the verdict on the row's escalation (Implementation, Merge) |
| `e` | switch to the admiral fleetd convened on the row's confirmed escalation |
| any other | run `handler.sh <tab/pane> <key> <row fields…>` on the focused row |

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
