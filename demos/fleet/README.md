# fleet architecture viewer

Interactive architecture / call-flow viewer for the **fleet itself** — the
factory's own machinery (`fleetd` / `fleetc` / the `ledger` / the crew of cogs),
*not* the product it builds. Same React Flow + crossing-aware orthogonal edge
router as `../architecture`, in the dark palette. The renderer
(`src/Topology.jsx`, `src/obstacleRouter.js`, `src/theme.css`) is a byte-identical
copy of the `architecture` viewer — only the graph data and the `kind` taxonomy
differ. Source only: the notes site (`site/`) serves it, on its own packages.

```bash
make serve           # → http://localhost:5175/fleet
```

- `graphs/fleet/overview.json` — the fleet at a glance (the default view):
  nodes + `base` (the standing edges, always drawn).
- `graphs/fleet/data.json` — the data model (`?graph=data`): the domain types
  in `crates/domain` beside every table in `ledger.db` (+ `telemetry.db`).
  Click a node for its Rust struct or SQL DDL.
- `src/Topology.jsx` — React Flow renderer; colours `kind` per the fleet
  taxonomy (daemon · adapter · sqlite · cog · cli · socket · …). Drag nodes to
  tune layout, then "Copy layout" to export the x/y back into the JSON.
- `src/obstacleRouter.js` — the orthogonal, obstacle-avoiding grid router.

To add a view, drop a graph under `graphs/fleet/` and register it in the
`GRAPHS` / `GROUPS` tables at the top of `src/Topology.jsx`.
