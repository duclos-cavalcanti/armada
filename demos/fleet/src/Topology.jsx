import { useMemo, useState } from 'react'
import {
  ReactFlow, Background, Controls, MiniMap, MarkerType,
  Handle, Position, BaseEdge, EdgeLabelRenderer, useNodesState,
} from '@xyflow/react'
import architecture from '../graphs/fleet/architecture.json'
import { routeOrthogonal, simplify, roundedPath, polylineMidpoint } from './obstacleRouter'

// renderable graphs share one schema (groups + x/y nodes + base edges). Pick
// which one to render via ?graph=<name>; defaults to the full architecture.
const GRAPHS = {
  architecture,
}
const GRAPH_LABELS = {
  architecture: 'Full architecture',
}
// two-level nav: the main dropdown picks a group; a multi-member group gets a
// second dropdown for the aspect within it. The group is derived from ?graph,
// so the URL stays graph-addressed. One graph today — the fleet topology —
// with flow overlays; add more under graphs/fleet/ and register them here.
const GROUPS = [
  { id: 'architecture', label: 'Full architecture', graphs: ['architecture'] },
]
const graphName = new URLSearchParams(window.location.search).get('graph') || 'architecture'
const arch = GRAPHS[graphName] ?? architecture
const activeGroup = GROUPS.find((g) => g.graphs.includes(graphName)) ?? GROUPS[0]

const MONO = 'ui-monospace, Menlo, monospace'

// Global "spread" factor: node POSITIONS (and group boxes) are scaled by this at
// render time while node SIZES stay fixed — so every graph gets wider gaps (room
// for edge labels) without touching the saved layouts. Copy-layout divides it
// back out so exported coords stay in the JSON's native (unscaled) space.
const SPREAD = 1.3

// fleet-component taxonomy → palette + glyph. The daemon's own parts, its
// driven adapters (ports), the SQLite ledger, and the crew of tmux cogs.
const KIND = {
  human:        { c: '#58A6FF', g: '👤' },  // the operator
  expert:       { c: '#D2A8FF', g: '🧭' },  // standing crew (admiral / shipwright)
  cli:          { c: '#79C0FF', g: '⌨'  },  // fleetc — the control surface
  tui:          { c: '#39C5CF', g: '▦'  },  // read-only client (dashboard)
  socket:       { c: '#9DA7B3', g: '⇄'  },  // the UDS control bus
  daemon:       { c: '#BC8CFF', g: '⬡'  },  // a fleetd module (server/scheduler/reconcile/sync/collector)
  adapter:      { c: '#D29922', g: '🔌' },  // a driven adapter (Forge/Tracker/SessionHost/git)
  sqlite:       { c: '#56D364', g: '▤'  },  // the ledger / telemetry DB
  cog:          { c: '#3FB950', g: '⚙'  },  // a crew cog — a Claude session in a tmux pane
  config:       { c: '#9DA7B3', g: '⚙'  },  // repo / hub
  external:     { c: '#DB6D28', g: '☁'  },  // GitLab / Jira / Claude Code
}

const LEGEND = [
  ['human', 'operator'], ['expert', 'standing crew'], ['cli', 'fleetc'], ['tui', 'read client'],
  ['socket', 'control bus'], ['daemon', 'fleetd module'], ['adapter', 'driven adapter'],
  ['sqlite', 'ledger / telemetry'], ['cog', 'crew cog (tmux)'], ['config', 'repo / hub'],
  ['external', 'external system'],
]

const EDGE = '#4d5864'
const SIDES = [['left', Position.Left], ['right', Position.Right], ['top', Position.Top], ['bottom', Position.Bottom]]
const hidden = { opacity: 0, pointerEvents: 'none', width: 1, height: 1, minWidth: 1, minHeight: 1, border: 0 }

function CompNode({ data }) {
  const k = KIND[data.kind] ?? KIND.config
  // custody-domain nodes render as centered pills (tree-node look); the rest
  // are left-aligned cards.
  const isDomain = data.kind === 'domain' || data.kind === 'domainnew'
  return (
    <div
      style={{
        width: data.w, height: data.h, boxSizing: 'border-box',
        background: 'var(--panel)', color: 'var(--text)',
        border: `${data.selected ? 2.4 : 1.6}px solid ${k.c}`,
        borderLeft: data.kind === 'table' ? `6px solid ${k.c}` : undefined,
        borderRadius: isDomain ? 999 : 10, padding: '6px 10px', cursor: 'pointer',
        textAlign: isDomain ? 'center' : 'left',
        display: isDomain ? 'flex' : undefined, flexDirection: isDomain ? 'column' : undefined,
        alignItems: isDomain ? 'center' : undefined, justifyContent: isDomain ? 'center' : undefined,
        boxShadow: data.selected ? `0 0 0 3px ${k.c}55, 0 0 18px -2px ${k.c}` : undefined,
        transition: 'box-shadow 160ms ease, opacity 160ms ease',
      }}
    >
      {SIDES.map(([n, p]) => <Handle key={'s' + n} id={'s-' + n} type="source" position={p} style={hidden} />)}
      {SIDES.map(([n, p]) => <Handle key={'t' + n} id={'t-' + n} type="target" position={p} style={hidden} />)}
      <div style={{ color: k.c, font: `700 10px ${MONO}`, letterSpacing: '0.05em' }}>{k.g} {data.label}</div>
      {data.sub && <div style={{ color: 'var(--dim)', font: '9px sans-serif', marginTop: 2 }}>{data.sub}</div>}
    </div>
  )
}

// an edge whose geometry was computed globally (crossing-aware) in routeAll
function RoutedEdge({ id, data, markerEnd, style }) {
  return (
    <>
      <BaseEdge id={id} path={data.path} markerEnd={markerEnd} style={style} />
      {data.label && !data.dim && (
        <EdgeLabelRenderer>
          <div
            className="nodrag nopan"
            style={{
              position: 'absolute',
              transform: `translate(-50%,-50%) translate(${data.labelPos.x}px,${data.labelPos.y}px)`,
              background: 'var(--bg)', border: '1px solid var(--border)', borderRadius: 6,
              padding: '1px 6px', font: `9px/1.4 ${MONO}`, color: 'var(--muted)',
              pointerEvents: 'none', whiteSpace: 'nowrap',
            }}
          >
            {data.label}
          </div>
        </EdgeLabelRenderer>
      )}
    </>
  )
}

const nodeTypes = { comp: CompNode }
const edgeTypes = { routed: RoutedEdge }

const PANE_H = { font: `700 10px ${MONO}`, letterSpacing: '0.1em', color: 'var(--dim)' }

function Field({ label, text }) {
  if (!text) return null
  return (
    <div style={{ marginTop: 12 }}>
      <div style={PANE_H}>{label}</div>
      <div style={{ font: '12px/1.6 sans-serif', color: 'var(--text)', marginTop: 4 }}>{text}</div>
    </div>
  )
}

// a DynamoDB item rendered as a record card: key-schema badges + attribute
// rows grouped by write op. Driven by a mode's optional `item` field.
function ItemCard({ item, accent }) {
  const Badge = ({ label, name }) => (
    <span style={{ display: 'inline-flex', gap: 5, alignItems: 'center', padding: '3px 8px', borderRadius: 6, background: `${accent}22`, border: `1px solid ${accent}66` }}>
      <span style={{ font: `700 9px ${MONO}`, color: accent, letterSpacing: '0.04em' }}>{label}</span>
      <span style={{ font: `11px ${MONO}`, color: 'var(--text)' }}>{name}</span>
    </span>
  )
  return (
    <div style={{ marginTop: 8 }}>
      <div style={{ border: '1px solid var(--border)', borderRadius: 8, overflow: 'hidden', background: 'var(--bg)' }}>
        {(item.pk || item.sk) && (
          <div style={{ display: 'flex', gap: 8, padding: '9px 10px', borderBottom: '1px solid var(--border)', flexWrap: 'wrap' }}>
            {item.pk && <Badge label="PK" name={item.pk} />}
            {item.sk && <Badge label="SK" name={item.sk} />}
          </div>
        )}
        {(item.ops ?? []).map((op, i) => (
          <div key={i} style={{ padding: '8px 10px', borderTop: i ? '1px solid var(--border)' : undefined }}>
            {op.op && <div style={{ font: `600 10px ${MONO}`, color: 'var(--dim)', marginBottom: 6, letterSpacing: '0.03em' }}>{op.op}</div>}
            {(op.attrs ?? []).map(([name, val], j) => (
              <div key={j} style={{ display: 'flex', justifyContent: 'space-between', gap: 14, padding: '2px 0', font: `11px/1.5 ${MONO}` }}>
                <span style={{ color: 'var(--text)' }}>{name}</span>
                <span style={{ color: 'var(--muted)', textAlign: 'right' }}>{val}</span>
              </div>
            ))}
          </div>
        ))}
      </div>
      {item.note && <div style={{ font: '11px/1.5 sans-serif', color: 'var(--dim)', marginTop: 6 }}>{item.note}</div>}
    </div>
  )
}

// right-hand inspector: a clicked component's modes + what happens inside
function DetailPane({ node, mode, setMode, onClose }) {
  const k = KIND[node.kind] ?? KIND.config
  const d = node.detail
  const m = d?.modes?.[mode]
  return (
    <div style={{
      position: 'fixed', top: 0, right: 0, width: 372, height: '100vh', zIndex: 7,
      background: 'var(--panel)', borderLeft: '1px solid var(--border)',
      boxShadow: '-10px 0 30px rgba(0,0,0,0.45)', overflowY: 'auto', padding: '18px 18px 48px',
    }}>
      <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center' }}>
        <span style={{ font: `700 10px ${MONO}`, letterSpacing: '0.14em', color: 'var(--dim)' }}>COMPONENT</span>
        <button onClick={onClose} title="Close (Esc)"
          style={{ background: 'none', border: 'none', color: 'var(--dim)', cursor: 'pointer', fontSize: 20, lineHeight: 1 }}>×</button>
      </div>
      <div style={{ marginTop: 12, color: k.c, font: `700 16px ${MONO}` }}>{k.g} {node.label}</div>
      <div style={{ color: 'var(--muted)', font: '12px sans-serif', marginTop: 3 }}>{d?.type ?? node.sub}</div>
      {d?.summary && <div style={{ color: 'var(--text)', font: '12px/1.6 sans-serif', marginTop: 14 }}>{d.summary}</div>}

      {d?.modes && (
        <div style={{ display: 'flex', gap: 6, marginTop: 16, flexWrap: 'wrap' }}>
          {d.modes.map((mm, i) => (
            <button key={i} onClick={() => setMode(i)}
              style={{
                font: `600 10px ${MONO}`, padding: '5px 10px', borderRadius: 6, cursor: 'pointer',
                background: i === mode ? k.c : 'transparent', color: i === mode ? 'var(--bg)' : 'var(--muted)',
                border: `1px solid ${i === mode ? k.c : 'var(--border)'}`,
              }}>{mm.label}</button>
          ))}
        </div>
      )}

      {m && (
        <div style={{ marginTop: 14 }}>
          {m.status && <div style={{ font: `10px ${MONO}`, color: 'var(--dim)', marginBottom: 6 }}>▸ {m.status}</div>}
          <Field label="GOAL" text={m.goal} />
          <Field label="WHY" text={m.why} />
          {m.steps && (
            <div style={{ marginTop: 16 }}>
              <div style={PANE_H}>WHAT HAPPENS INSIDE</div>
              <ol style={{ margin: '10px 0 0', padding: 0, listStyle: 'none' }}>
                {m.steps.map((s, i) => (
                  <li key={i} style={{ display: 'flex', gap: 9, marginBottom: 9 }}>
                    <span style={{ flex: '0 0 18px', height: 18, borderRadius: 4, background: `${k.c}22`, color: k.c, font: `700 10px ${MONO}`, display: 'flex', alignItems: 'center', justifyContent: 'center' }}>{i + 1}</span>
                    <span style={{ font: '12px/1.5 sans-serif', color: 'var(--text)' }}>{s}</span>
                  </li>
                ))}
              </ol>
            </div>
          )}
          {m.item && (
            <div style={{ marginTop: 16 }}>
              <div style={PANE_H}>{m.shapeLabel ?? 'ITEM'}</div>
              <ItemCard item={m.item} accent={k.c} />
            </div>
          )}
          {m.shape && (
            <div style={{ marginTop: 16 }}>
              <div style={PANE_H}>{m.shapeLabel ?? 'SHAPE'}</div>
              <pre style={{
                margin: '8px 0 0', padding: '10px 12px', overflowX: 'auto',
                background: 'var(--bg)', border: '1px solid var(--border)', borderRadius: 8,
                font: `11px/1.5 ${MONO}`, color: 'var(--text)', whiteSpace: 'pre',
              }}>{m.shape}</pre>
            </div>
          )}
          {m.guarantees && (
            <div style={{ marginTop: 16 }}>
              <div style={PANE_H}>GUARANTEES</div>
              {m.guarantees.map((g, i) => (
                <div key={i} style={{ font: '12px/1.5 sans-serif', color: 'var(--muted)', marginTop: 6 }}>{g}</div>
              ))}
            </div>
          )}
        </div>
      )}

      {!d && <div style={{ marginTop: 16, font: '12px/1.6 sans-serif', color: 'var(--dim)' }}>No detailed modes documented yet for this component.</div>}
    </div>
  )
}

// which side each box attaches on, from relative geometry
function pickSides(a, b) {
  const dx = (b.x + b.w / 2) - (a.x + a.w / 2)
  const dy = (b.y + b.h / 2) - (a.y + a.h / 2)
  if (Math.abs(dx) >= Math.abs(dy)) return dx >= 0 ? ['right', 'left'] : ['left', 'right']
  return dy >= 0 ? ['bottom', 'top'] : ['top', 'bottom']
}

// anchor point on a box side, offset by frac ∈ [-0.5,0.5] along that side so
// several edges sharing a side fan out instead of stacking on one point
function anchor(r, side, frac = 0) {
  if (side === 'left') return { x: r.x, y: r.y + r.h * (0.5 + frac) }
  if (side === 'right') return { x: r.x + r.w, y: r.y + r.h * (0.5 + frac) }
  if (side === 'top') return { x: r.x + r.w * (0.5 + frac), y: r.y }
  return { x: r.x + r.w * (0.5 + frac), y: r.y + r.h }
}

const manhattan = (a, b) => Math.abs(a.x - b.x) + Math.abs(a.y - b.y)

// orthogonal last-resort path (never a diagonal) when the grid router can't
// thread a route even at tight clearance
function elbow(a, b, ss, ts) {
  const sh = ss === 'left' || ss === 'right'
  const th = ts === 'left' || ts === 'right'
  if (sh && th) { const mx = (a.x + b.x) / 2; return [a, { x: mx, y: a.y }, { x: mx, y: b.y }, b] }
  if (!sh && !th) { const my = (a.y + b.y) / 2; return [a, { x: a.x, y: my }, { x: b.x, y: my }, b] }
  if (sh && !th) return [a, { x: b.x, y: a.y }, b]
  return [a, { x: a.x, y: b.y }, b]
}

// route with progressively tighter clearance before falling back to an elbow
function routeWithRetries(a, b, obstacles, ss, ts, occupied) {
  const base = { sourceDir: ss, targetDir: ts, occupied }
  const pts =
    routeOrthogonal(a.x, a.y, b.x, b.y, obstacles, base) ||
    routeOrthogonal(a.x, a.y, b.x, b.y, obstacles, { ...base, clear: 8, stub: 12 }) ||
    routeOrthogonal(a.x, a.y, b.x, b.y, obstacles, { ...base, clear: 2, stub: 6 })
  return pts ? simplify(pts) : elbow(a, b, ss, ts)
}

// route every edge together: longer backbones claim lanes first, each later
// edge steers around boxes AND the tracks earlier edges already took.
function routeAll(nodeList) {
  const rect = {}
  for (const n of nodeList) if (n.type === 'comp') rect[n.id] = { x: n.position.x, y: n.position.y, w: n.data.w, h: n.data.h }

  const meta = arch.base
    .filter((e) => rect[e.source] && rect[e.target])
    .map((e) => { const [ss, ts] = pickSides(rect[e.source], rect[e.target]); return { e, ss, ts } })

  // spread anchors per (node, side), ordered by where each edge's *far* end
  // sits along that side — so a fan of edges leaving one side never crosses
  // itself (the middle-bound arrow exits the middle, etc.)
  const groups = {}
  meta.forEach((m, i) => {
    ;(groups[`${m.e.source}|${m.ss}`] ??= []).push(`${i}|s`)
    ;(groups[`${m.e.target}|${m.ts}`] ??= []).push(`${i}|t`)
  })
  const oppCoord = (key, axis) => {
    const [i, end] = key.split('|')
    const m = meta[+i]
    const r = end === 's' ? rect[m.e.target] : rect[m.e.source]
    return axis === 'x' ? r.x + r.w / 2 : r.y + r.h / 2
  }
  const frac = {}
  for (const k in groups) {
    const side = k.split('|')[1]
    const axis = side === 'left' || side === 'right' ? 'y' : 'x'
    const g = groups[k].slice().sort((a, b) => oppCoord(a, axis) - oppCoord(b, axis))
    const n = g.length
    g.forEach((key, j) => { frac[key] = n === 1 ? 0 : (j / (n - 1) - 0.5) * 0.6 })
  }
  const srcA = (i) => anchor(rect[meta[i].e.source], meta[i].ss, frac[`${i}|s`])
  const tgtA = (i) => anchor(rect[meta[i].e.target], meta[i].ts, frac[`${i}|t`])

  const order = meta.map((_, i) => i).sort((a, b) => manhattan(srcA(b), tgtA(b)) - manhattan(srcA(a), tgtA(a)))

  const occupied = []
  const pathsById = {}
  for (const i of order) {
    const { e, ss, ts } = meta[i]
    const a = srcA(i), b = tgtA(i)
    const obstacles = Object.entries(rect).filter(([id]) => id !== e.source && id !== e.target).map(([, r]) => r)
    const poly = routeWithRetries(a, b, obstacles, ss, ts, occupied)
    occupied.push(poly)
    pathsById[i] = poly
  }

  return meta.map(({ e, ss, ts }, i) => ({
    id: `be${i}`, source: e.source, target: e.target,
    sourceHandle: `s-${ss}`, targetHandle: `t-${ts}`,
    type: 'routed',
    data: { path: roundedPath(pathsById[i]), label: e.label, labelPos: polylineMidpoint(pathsById[i]) },
    markerEnd: { type: MarkerType.ArrowClosed, color: EDGE, width: 13, height: 13 },
    style: { stroke: EDGE, strokeWidth: 1.2 },
  }))
}

export default function Topology() {
  const initialNodes = useMemo(() => {
    const groupNodes = arch.groups.map((grp) => ({
      id: `grp-${grp.id}`,
      position: { x: grp.x * SPREAD, y: grp.y * SPREAD },
      data: { label: <div style={{ position: 'absolute', top: 6, left: 10, font: `700 11px ${MONO}`, letterSpacing: '0.06em', color: grp.color, textTransform: 'uppercase' }}>{grp.label}</div> },
      style: { width: grp.w * SPREAD, height: grp.h * SPREAD, background: `${grp.color}0c`, border: `1px dashed ${grp.color}55`, borderRadius: 14 },
      selectable: false, draggable: false, zIndex: 0,
    }))
    const compNodes = arch.nodes.map((n) => ({
      id: n.id, type: 'comp', position: { x: n.x * SPREAD, y: n.y * SPREAD },
      data: { kind: n.kind, label: n.label, sub: n.sub, w: n.w, h: n.h }, zIndex: 1,
    }))
    return [...groupNodes, ...compNodes]
  }, [])

  const [nodes, , onNodesChange] = useNodesState(initialNodes)
  const edges = useMemo(() => routeAll(nodes), [nodes])
  const [copied, setCopied] = useState(false)
  const [selected, setSelected] = useState(null)
  const [mode, setMode] = useState(0)
  const sel = selected ? arch.nodes.find((n) => n.id === selected) : null

  // when something is selected, spotlight it: the picked node glows, everything
  // else fades. edges touching the selection stay lit; the rest recede.
  const viewNodes = useMemo(() => nodes.map((n) => {
    if (n.type !== 'comp') return n
    const dim = selected && n.id !== selected
    return { ...n, data: { ...n.data, selected: n.id === selected }, style: { ...n.style, opacity: dim ? 0.28 : 1 } }
  }), [nodes, selected])

  const viewEdges = useMemo(() => edges.map((e) => {
    const lit = !selected || e.source === selected || e.target === selected
    return { ...e, style: { ...e.style, opacity: lit ? 1 : 0.1, transition: 'opacity 160ms ease' }, data: { ...e.data, dim: !lit } }
  }), [edges, selected])

  // dump the current (dragged) node positions merged back into the source
  // node list, so a hand-tuned arrangement can be pasted into architecture.json
  const copyLayout = async () => {
    const pos = {}
    for (const n of nodes) if (n.type === 'comp') pos[n.id] = { x: Math.round(n.position.x / SPREAD), y: Math.round(n.position.y / SPREAD) }
    const merged = arch.nodes.map((n) => ({ ...n, x: pos[n.id]?.x ?? n.x, y: pos[n.id]?.y ?? n.y }))
    const text = JSON.stringify(merged)
    try { await navigator.clipboard.writeText(text) } catch { /* clipboard blocked — fall back to console */ }
    console.log(text)
    setCopied(true)
    setTimeout(() => setCopied(false), 1500)
  }

  return (
    <div style={{ width: '100vw', height: '100vh', position: 'relative' }}>
      <div className="viz-header">
        <b>{arch.title ?? 'Armada — architecture topology'}</b>
        <span style={{ color: 'var(--dim)', fontSize: 11, display: 'block', marginTop: 4 }}>crossing-aware orthogonal routing · baseline @ {arch.sha}</span>
      </div>
      <div style={{ position: 'fixed', top: 14, right: 14, zIndex: 6, display: 'flex', gap: 8, alignItems: 'center' }}>
        {activeGroup.graphs.length > 1 && (
          <select
            value={graphName}
            onChange={(e) => { window.location.search = '?graph=' + e.target.value }}
            title="Pick the aspect within this group"
            style={{
              background: 'var(--panel)', color: 'var(--text)', border: '1px solid var(--border)',
              borderRadius: 8, padding: '7px 10px', cursor: 'pointer', font: `600 11px ${MONO}`,
            }}
          >
            {activeGroup.graphs.map((k) => <option key={k} value={k}>{GRAPH_LABELS[k] ?? k}</option>)}
          </select>
        )}
        <select
          value={activeGroup.id}
          onChange={(e) => { window.location.search = '?graph=' + GROUPS.find((g) => g.id === e.target.value).graphs[0] }}
          title="Switch diagram group"
          style={{
            background: 'var(--panel)', color: 'var(--text)', border: '1px solid var(--border)',
            borderRadius: 8, padding: '7px 10px', cursor: 'pointer', font: `600 11px ${MONO}`,
          }}
        >
          {GROUPS.map((g) => <option key={g.id} value={g.id}>{g.label}</option>)}
        </select>
        <button
          onClick={copyLayout}
          title="Copy the current node positions as JSON (paste into the graph's nodes)"
          style={{
            background: copied ? 'var(--green)' : 'var(--panel)',
            color: copied ? 'var(--bg)' : 'var(--text)',
            border: `1px solid ${copied ? 'var(--green)' : 'var(--border)'}`,
            borderRadius: 8, padding: '7px 12px', cursor: 'pointer',
            font: `600 11px ${MONO}`, letterSpacing: '0.04em',
          }}
        >
          {copied ? 'Copied ✓' : 'Layout'}
        </button>
      </div>
      <ReactFlow
        nodes={viewNodes} edges={viewEdges} onNodesChange={onNodesChange}
        nodeTypes={nodeTypes} edgeTypes={edgeTypes}
        onNodeClick={(_, n) => { if (n.type === 'comp') { setSelected(n.id); setMode(0) } }}
        onPaneClick={() => setSelected(null)}
        fitView minZoom={0.1} nodesDraggable
        proOptions={{ hideAttribution: true }}
      >
        <Background color="#30363D" gap={22} size={1} />
        <Controls />
        <MiniMap pannable zoomable maskColor="rgba(13,17,23,0.7)"
          nodeColor={(n) => (KIND[kindOf(n.id)] ?? KIND.config).c} />
      </ReactFlow>
      <div style={{ position: 'fixed', bottom: 14, right: 14, zIndex: 6, background: 'var(--panel)', border: '1px solid var(--border)', borderRadius: 8, padding: '8px 10px', display: 'grid', gridTemplateColumns: '1fr 1fr', gap: '3px 12px' }}>
        {LEGEND.map(([k, label]) => (
          <span key={k} style={{ font: `10px ${MONO}`, color: 'var(--muted)', display: 'flex', alignItems: 'center', gap: 6 }}>
            <span style={{ width: 9, height: 9, borderRadius: 2, background: (KIND[k] ?? KIND.config).c, display: 'inline-block' }} />{label}
          </span>
        ))}
      </div>
      {sel && <DetailPane node={sel} mode={mode} setMode={setMode} onClose={() => setSelected(null)} />}
    </div>
  )
}

function kindOf(id) {
  const n = arch.nodes.find((a) => a.id === id)
  return n?.kind
}
