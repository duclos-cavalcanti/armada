# dashboard

A live TUI over the fleet. Reads **fleetd's ledger read-only**
(the project's `.fleet/ledger.db` — the project enclosing the cwd, or `-C <dir>`) and renders the fleet at a glance,
refreshing once a second. A **viewer, not an orchestrator** — it renders state and
binds keys to copy it out; it never mutates the fleet (that's `fleetc`). Built on
[ratatui](https://ratatui.rs) + [crossterm](https://crates.io/crates/crossterm) — a
single static binary, no runtime dependencies.

## Tabs

Cycle with `Tab` / `S-Tab`:

- **Fleet** — the line, top-down by how far each ticket has got; each ticket sits in one pane:
  - **Dispatch** — orders waiting for their builder (held ✋ / blocked ⛔ / not yet spawned);
  - **Implementation** — builders at work (workers + hands): state, liveness, MR + pipeline, last action;
  - **Merge** — our ships being landed: the sailor's state (dimmed once it's stood down),
    pipeline, approvals, armed / on a merge train, open threads, what it's blocked on, merge status;
- **Ships** — every open MR fleetd tracks, ours + allies; the title shows the lookout.
- **Board** — the sprint tickets + convoys (a convoy shows its captain while one is live).

## Keys

| Key | Action |
|---|---|
| `q` / `Esc` | quit |
| `r` | refresh now |
| `Tab` / `S-Tab` | next / previous tab |
| `↑` / `↓`, `C-p` / `C-n` | previous / next pane |
| `1`–`3` | focus the Nth pane |
| `j` / `k` | move the cursor within the focused pane |
| `space` | zoom the focused pane (toggle) |
| `y` / `Y` | yank the focused pane / the whole page |
| any other | run `handler.sh <tab/pane> <key> <row fields…>` on the focused row |

## Layout

```
apps/dashboard/
├── Cargo.toml
├── handler.sh       # the action handler: (tab/pane, key) → action
└── src/
    ├── main.rs      # entry, event loop, key handling, tab + pane focus
    ├── action.rs    # non-navigation keys → handler.sh (the action handler)
    ├── source.rs    # Fleet::load() — reads the ledger read-only into the view model
    └── ui/mod.rs    # tab strip + per-tab panes + footer (pure render over App)
```

## Build & run

```bash
cargo run -p dashboard -- -C projects/<name>         # or run it from inside the project
cargo run -p dashboard -- --ledger /path/to/ledger.db
```
