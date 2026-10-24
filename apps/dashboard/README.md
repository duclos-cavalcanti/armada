# dashboard

A live TUI over the MADY fleet. Reads **fleetd's ledger read-only**
(`$HOME/.fleet/ledger.db`, or `--ledger <path>`) and renders the fleet at a glance,
refreshing once a second. A **viewer, not an orchestrator** — it renders state and
binds keys to copy it out; it never mutates the fleet (that's `fleetc`). Built on
[ratatui](https://ratatui.rs) + [crossterm](https://crates.io/crates/crossterm) — a
single static binary, no runtime dependencies.

## Tabs

Cycle with `Tab` / `S-Tab`:

- **Fleet** — the live cog roster over the open escalations.
- **Armada** — the open ships (MRs) fleetd tracks.
- **Board** — the sprint tickets + convoys.

## Keys

| Key | Action |
|---|---|
| `q` / `Esc` | quit |
| `r` | refresh now |
| `Tab` / `S-Tab` | next / previous tab |
| `↑` / `↓` | move focus between panes |
| `1` / `2` | focus the Nth pane |
| `j` / `k` | move the cursor within the focused pane |
| `space` | zoom the focused pane (toggle) |
| `y` / `Y` | yank the focused pane / the whole page |

## Layout

```
apps/dashboard/
├── Cargo.toml
└── src/
    ├── main.rs      # entry, event loop, key handling, tab + pane focus
    ├── config.rs    # CLI + defaults (the ledger path)
    ├── source.rs    # Fleet::load() — reads the ledger read-only into the view model
    └── ui/mod.rs    # tab strip + per-tab panes + footer (pure render over App)
```

## Build & run

```bash
cargo run -p dashboard                               # defaults to ~/.fleet/ledger.db
cargo run -p dashboard -- --ledger /path/to/ledger.db
```
