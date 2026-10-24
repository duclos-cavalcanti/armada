# fleet architecture viewer

Interactive architecture / call-flow viewer for the **fleet itself** — the
factory's own machinery (`fleetd` / `fleetc` / the `ledger` / the crew of cogs),
*not* the b2b-api product. Same React Flow + crossing-aware orthogonal edge
router as `../architecture`, in the mady-style dark palette. The renderer
(`src/Topology.jsx`, `src/obstacleRouter.js`, `src/theme.css`) is a byte-identical
copy of the `architecture` viewer — only the graph data and the `kind` taxonomy
differ. Tracked app (source only; `node_modules/` + `dist/` are gitignored).

```bash
cd demos/fleet
npm install
npm run dev          # Vite dev server → http://localhost:5173
```

- `graphs/fleet/architecture.json` — the fleet topology: nodes + `base` (the
  standing edges, always drawn) + `flows` (selectable overlays — dispatch,
  build→handoff, sail→merge, sync tick, convoy, lookout, anchor, telemetry).
  Click `scheduler` / `reconcile` for their per-mode detail panes.
- `src/Topology.jsx` — React Flow renderer; colours `kind` per the fleet
  taxonomy (daemon · adapter · sqlite · cog · cli · socket · …). Drag nodes to
  tune layout, then "Copy layout" to export the x/y back into the JSON.
- `src/obstacleRouter.js` — the orthogonal, obstacle-avoiding grid router.

To add a view, drop a graph under `graphs/fleet/` and register it in the
`GRAPHS` / `GROUPS` tables at the top of `src/Topology.jsx`.
