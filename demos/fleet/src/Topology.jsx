import { useMemo, useState } from 'react'
import {
  ReactFlow, Background, Controls, MiniMap, MarkerType,
  Handle, Position, BaseEdge, EdgeLabelRenderer, useNodesState,
  NodeResizer, useReactFlow,
} from '@xyflow/react'
import architecture from '../graphs/fleet/architecture.json'
import overview from '../graphs/fleet/overview.json'
import data from '../graphs/fleet/data.json'
import { toPng } from 'html-to-image'
import { routeOrthogonal, simplify, roundedPath, polylineMidpoint } from './obstacleRouter'

// renderable graphs share one schema (groups + x/y nodes + base edges). Pick
// which one to render via ?graph=<name>; defaults to the full architecture.
const GRAPHS = {
  overview,
  architecture,
  data,
}
const GRAPH_LABELS = {
  overview: 'Overview',
  architecture: 'Full architecture',
  data: 'Data model',
}
// two-level nav: the main dropdown picks a group; a multi-member group gets a
// second dropdown for the aspect within it. The group is derived from ?graph,
// so the URL stays graph-addressed. One graph today — the fleet topology —
// with flow overlays; add more under graphs/fleet/ and register them here.
const GROUPS = [
  { id: 'overview', label: 'Overview', graphs: ['overview'] },
  { id: 'architecture', label: 'Full architecture', graphs: ['architecture'] },
  { id: 'data', label: 'Data model', graphs: ['data'] },
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
  // data-diagram taxonomy (the `data` graph): rust domain types + ledger tables.
  struct:       { c: '#D2A8FF', g: '◆'  },  // domain entity (a persisted Rust struct)
  dto:          { c: '#39C5CF', g: '◇'  },  // read-model / port DTO (never a table)
  enum:         { c: '#F0883E', g: '▸'  },  // domain enum (a vocabulary)
  table:        { c: '#56D364', g: '▤'  },  // a ledger.db / telemetry.db table
  skill:        { c: '#D2A8FF', g: '✦'  },  // an operator /fleet-* skill (judgment over fleetc)
}

const LEGEND = [
  ['human', 'operator'], ['expert', 'standing crew'], ['cli', 'fleetc'], ['tui', 'read client'],
  ['socket', 'control bus'], ['daemon', 'fleetd module'], ['adapter', 'driven adapter'],
  ['sqlite', 'ledger / telemetry'], ['cog', 'crew cog (tmux)'], ['config', 'repo / hub'],
  ['external', 'external system'],
  ['struct', 'domain struct'], ['dto', 'read-model / DTO'], ['enum', 'domain enum'], ['table', 'ledger table'],
  ['skill', 'operator skill (/fleet-*)'],
]
// only show legend rows for kinds actually present in the active graph, so each
// graph's legend stays relevant (architecture/overview never show the data kinds).
const PRESENT_KINDS = new Set(arch.nodes.map((n) => n.kind))
const VISIBLE_LEGEND = LEGEND.filter(([k]) => PRESENT_KINDS.has(k))

const EDGE = '#4d5864'
const SIDES = [['left', Position.Left], ['right', Position.Right], ['top', Position.Top], ['bottom', Position.Bottom]]
const hidden = { opacity: 0, pointerEvents: 'none', width: 1, height: 1, minWidth: 1, minHeight: 1, border: 0 }

function CompNode({ id, data }) {
  const k = KIND[data.kind] ?? KIND.config
  const { setNodes } = useReactFlow()
  // custody-domain nodes render as centered pills (tree-node look); the rest
  // are left-aligned cards.
  const isDomain = data.kind === 'domain' || data.kind === 'domainnew'
  return (
    <>
      {/* drag-to-resize handles on the selected card; writes the new size back
          into data.w/h so routeAll reroutes and "Layout" can export it. */}
      <NodeResizer
        isVisible={!!data.selected}
        minWidth={70}
        minHeight={26}
        color={k.c}
        handleStyle={{ width: 7, height: 7, borderRadius: 2 }}
        onResize={(_, p) => setNodes((ns) => ns.map((n) => (n.id === id ? { ...n, data: { ...n.data, w: Math.round(p.width), h: Math.round(p.height) } } : n)))}
      />
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
    </>
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

// a group rectangle. Rendered behind the comp nodes; unlike a plain box it
// carries the same hidden side-handles, so an edge can target the GROUP itself
// (e.g. one arrow into the whole SKILLS cluster) instead of a member node.
function GroupNode({ data }) {
  return (
    <div style={{
      width: '100%', height: '100%', position: 'relative',
      background: `${data.color}0c`, border: `1px dashed ${data.color}55`, borderRadius: 14,
    }}>
      {SIDES.map(([n, p]) => <Handle key={'s' + n} id={'s-' + n} type="source" position={p} style={hidden} />)}
      {SIDES.map(([n, p]) => <Handle key={'t' + n} id={'t-' + n} type="target" position={p} style={hidden} />)}
      {/* label as a chip on the top border: a solid dark bg keeps it legible over
          the tint and clear of any comp card that sits near the box edge. */}
      <div style={{
        position: 'absolute', top: -11, left: 8, font: `700 10px ${MONO}`, letterSpacing: '0.08em',
        color: data.color, textTransform: 'uppercase', whiteSpace: 'nowrap',
        background: 'var(--bg)', padding: '2px 8px', borderRadius: 6, border: `1px solid ${data.color}66`,
      }}>{data.label}</div>
    </div>
  )
}

const nodeTypes = { comp: CompNode, grouprect: GroupNode }
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

// PoC: a small inline-SVG state machine for the side-pane — reveals the
// intricate internal loop an "opaque" node hides on the diagram surface.
// spec: { note?, w, h, states:[{id,label,sub?,x,y,w,h,accent?}],
//         transitions:[{from,to,label?,bidir?}] }
function StateMachine({ spec, accent }) {
  const by = Object.fromEntries(spec.states.map((s) => [s.id, s]))
  const center = (s) => ({ x: s.x + s.w / 2, y: s.y + s.h / 2 })
  // point on box border along the ray toward (tx,ty)
  const border = (s, tx, ty) => {
    const c = center(s), dx = tx - c.x, dy = ty - c.y
    const sx = dx ? (s.w / 2 + 4) / Math.abs(dx) : Infinity
    const sy = dy ? (s.h / 2 + 4) / Math.abs(dy) : Infinity
    const t = Math.min(sx, sy)
    return { x: c.x + dx * t, y: c.y + dy * t }
  }
  return (
    <div style={{ marginTop: 10 }}>
      <svg viewBox={`0 0 ${spec.w} ${spec.h}`} width="100%" style={{ display: 'block' }}>
        <defs>
          <marker id="sm-arrow" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse">
            <path d="M0,0 L10,5 L0,10 z" fill="var(--muted)" />
          </marker>
        </defs>
        {spec.transitions.map((t, i) => {
          const a = by[t.from], b = by[t.to]
          const ca = center(a), cb = center(b)
          const pa = border(a, cb.x, cb.y), pb = border(b, ca.x, ca.y)
          const mx = (pa.x + pb.x) / 2, my = (pa.y + pb.y) / 2
          const wLbl = (t.label?.length ?? 0) * 4.6 + 8
          return (
            <g key={i}>
              <line x1={pa.x} y1={pa.y} x2={pb.x} y2={pb.y} stroke="var(--muted)" strokeWidth="1.1"
                markerEnd="url(#sm-arrow)" markerStart={t.bidir ? 'url(#sm-arrow)' : undefined} />
              {t.label && (
                <>
                  <rect x={mx - wLbl / 2} y={my - 7} width={wLbl} height={14} rx={3} fill="var(--bg)" stroke="var(--border)" />
                  <text x={mx} y={my + 3} textAnchor="middle" fill="var(--muted)" style={{ font: `8px ${MONO}` }}>{t.label}</text>
                </>
              )}
            </g>
          )
        })}
        {spec.states.map((s) => {
          const c = s.accent ?? accent
          return (
            <g key={s.id}>
              <rect x={s.x} y={s.y} width={s.w} height={s.h} rx={8} fill="var(--panel)" stroke={c} strokeWidth="1.5" />
              <text x={s.x + s.w / 2} y={s.y + (s.sub ? s.h / 2 - 2 : s.h / 2 + 3)} textAnchor="middle" fill={c} style={{ font: `700 10px ${MONO}` }}>{s.label}</text>
              {s.sub && <text x={s.x + s.w / 2} y={s.y + s.h / 2 + 10} textAnchor="middle" fill="var(--dim)" style={{ font: `8px ${MONO}` }}>{s.sub}</text>}
            </g>
          )
        })}
      </svg>
      {spec.note && <div style={{ font: '11px/1.5 sans-serif', color: 'var(--dim)', marginTop: 6 }}>{spec.note}</div>}
    </div>
  )
}

// a grouped key list for config-style nodes: section → rows of
// { key, meta?, informs }. Scannable: accent key + a dim type/default pill +
// a muted one-liner, each row ruled by a left accent bar.
function FieldList({ sections, accent }) {
  return (
    <div style={{ marginTop: 6 }}>
      {sections.map((sec, si) => (
        <div key={si} style={{ marginTop: si ? 16 : 8 }}>
          <div style={{ font: `700 10px ${MONO}`, letterSpacing: '0.08em', color: 'var(--dim)', textTransform: 'uppercase' }}>{sec.section}</div>
          <div style={{ marginTop: 7, display: 'flex', flexDirection: 'column', gap: 9 }}>
            {sec.rows.map((r, ri) => (
              <div key={ri} style={{ borderLeft: `2px solid ${accent}66`, paddingLeft: 9 }}>
                <div style={{ display: 'flex', alignItems: 'baseline', gap: 7, flexWrap: 'wrap' }}>
                  <span style={{ font: `700 11px ${MONO}`, color: accent }}>{r.key}</span>
                  {r.meta && <span style={{ font: `9px ${MONO}`, color: 'var(--dim)', background: 'var(--bg)', border: '1px solid var(--border)', borderRadius: 4, padding: '0 5px' }}>{r.meta}</span>}
                </div>
                <div style={{ font: '11px/1.5 sans-serif', color: 'var(--muted)', marginTop: 2 }}>{r.informs}</div>
              </div>
            ))}
          </div>
        </div>
      ))}
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

      {d?.stateMachine && (
        <div style={{ marginTop: 16 }}>
          <div style={PANE_H}>INTERNAL STATE MACHINE</div>
          <StateMachine spec={d.stateMachine} accent={k.c} />
        </div>
      )}

      {d?.fields && (
        <div style={{ marginTop: 16 }}>
          <div style={PANE_H}>ALLOWED KEYS</div>
          <FieldList sections={d.fields} accent={k.c} />
        </div>
      )}

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
  // group boxes are valid edge ENDPOINTS (an arrow can land on the whole box),
  // but never OBSTACLES (edges pass through them freely). `geo` is the endpoint
  // lookup; obstacles below stay keyed off `rect` (comp nodes only).
  const geo = { ...rect }
  for (const n of nodeList) if (n.type === 'grouprect') geo[n.id] = { x: n.position.x, y: n.position.y, w: parseFloat(n.style.width), h: parseFloat(n.style.height) }

  const meta = arch.base
    .filter((e) => geo[e.source] && geo[e.target])
    .map((e) => { const [ss, ts] = pickSides(geo[e.source], geo[e.target]); return { e, ss, ts } })

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
    const r = end === 's' ? geo[m.e.target] : geo[m.e.source]
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
  const srcA = (i) => anchor(geo[meta[i].e.source], meta[i].ss, frac[`${i}|s`])
  const tgtA = (i) => anchor(geo[meta[i].e.target], meta[i].ts, frac[`${i}|t`])

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
    style: { stroke: EDGE, strokeWidth: 1.2, ...(e.dashed ? { strokeDasharray: '7 5' } : {}) },
  }))
}

export default function Topology() {
  const initialNodes = useMemo(() => {
    const groupNodes = arch.groups.map((grp) => ({
      id: `grp-${grp.id}`, type: 'grouprect',
      position: { x: grp.x * SPREAD, y: grp.y * SPREAD },
      data: { label: grp.label, color: grp.color },
      style: { width: grp.w * SPREAD, height: grp.h * SPREAD },
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
  const [exporting, setExporting] = useState(false)
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
    // positions are SPREAD-scaled at render; sizes (w/h) are native px, so only x/y divide back.
    for (const n of nodes) if (n.type === 'comp') pos[n.id] = { x: Math.round(n.position.x / SPREAD), y: Math.round(n.position.y / SPREAD), w: n.data.w, h: n.data.h }
    const merged = arch.nodes.map((n) => { const p = pos[n.id]; return p ? { ...n, x: p.x, y: p.y, w: p.w, h: p.h } : n })
    const text = JSON.stringify(merged)
    try { await navigator.clipboard.writeText(text) } catch { /* clipboard blocked — fall back to console */ }
    console.log(text)
    setCopied(true)
    setTimeout(() => setCopied(false), 1500)
  }

  // export the whole graph as a PNG. We size the capture to the full bounds of
  // the live nodes (comp cards + group boxes) and reset the viewport transform
  // on the CLONE only (html-to-image's `style` applies to the clone), so the
  // export always holds the entire diagram regardless of current zoom/pan.
  const downloadPng = async () => {
    const vp = document.querySelector('.react-flow__viewport')
    if (!vp) return
    let minX = Infinity, minY = Infinity, maxX = -Infinity, maxY = -Infinity
    for (const n of nodes) {
      const w = n.type === 'comp' ? n.data.w : parseFloat(n.style?.width) || 0
      const h = n.type === 'comp' ? n.data.h : parseFloat(n.style?.height) || 0
      minX = Math.min(minX, n.position.x); minY = Math.min(minY, n.position.y)
      maxX = Math.max(maxX, n.position.x + w); maxY = Math.max(maxY, n.position.y + h)
    }
    const pad = 48
    const width = Math.ceil(maxX - minX + pad * 2)
    const height = Math.ceil(maxY - minY + pad * 2)
    setExporting(true)
    try {
      const url = await toPng(vp, {
        backgroundColor: '#0D1117', pixelRatio: 2, width, height,
        style: { width: `${width}px`, height: `${height}px`, transform: `translate(${pad - minX}px, ${pad - minY}px) scale(1)` },
      })
      const a = document.createElement('a')
      a.download = `fleet-${graphName}.png`
      a.href = url
      a.click()
    } catch (err) {
      console.error('PNG export failed', err)
    } finally {
      setExporting(false)
    }
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
        <button
          onClick={downloadPng}
          disabled={exporting}
          title="Export the whole diagram as a PNG image"
          style={{
            background: 'var(--panel)', color: 'var(--text)',
            border: '1px solid var(--border)', borderRadius: 8,
            padding: '7px 12px', cursor: exporting ? 'default' : 'pointer',
            font: `600 11px ${MONO}`, letterSpacing: '0.04em', opacity: exporting ? 0.6 : 1,
          }}
        >
          {exporting ? 'Exporting…' : 'PNG'}
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
        {VISIBLE_LEGEND.map(([k, label]) => (
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
