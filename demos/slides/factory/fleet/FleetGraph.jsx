// Read-only embed of the fleet viewer's renderer (vendored from
// demos/fleet/src/Topology.jsx) — same crossing-aware orthogonal routing and
// node/group/edge look, with all the editing UI (dropdowns, layout/PNG,
// resizer, detail pane) stripped. Takes a graph ({ groups, nodes, base }) and
// renders it static inside a slide. Theme vars are set locally so it carries
// the mady-dark palette wherever it's embedded.
import { useMemo } from 'react';
import {
  ReactFlow, Background, Controls, MarkerType, Handle, Position, BaseEdge, EdgeLabelRenderer,
} from '@xyflow/react';
import '@xyflow/react/dist/style.css'; // required — without it .react-flow has no height and renders blank
import { routeOrthogonal, simplify, roundedPath, polylineMidpoint } from './obstacleRouter';

const MONO = 'ui-monospace, Menlo, monospace';
const SPREAD = 1.3;
const EDGE = '#4d5864';

const KIND = {
  human:   { c: '#58A6FF', g: '👤' },
  expert:  { c: '#D2A8FF', g: '🧭' },
  cli:     { c: '#79C0FF', g: '⌨'  },
  tui:     { c: '#39C5CF', g: '▦'  },
  socket:  { c: '#9DA7B3', g: '⇄'  },
  daemon:  { c: '#BC8CFF', g: '⬡'  },
  adapter: { c: '#D29922', g: '🔌' },
  sqlite:  { c: '#56D364', g: '▤'  },
  cog:     { c: '#3FB950', g: '⚙'  },
  config:  { c: '#9DA7B3', g: '⚙'  },
  external:{ c: '#DB6D28', g: '☁'  },
  struct:  { c: '#D2A8FF', g: '◆'  },
  dto:     { c: '#39C5CF', g: '◇'  },
  enum:    { c: '#F0883E', g: '▸'  },
  table:   { c: '#56D364', g: '▤'  },
  skill:   { c: '#D2A8FF', g: '✦'  },
};

const SIDES = [['left', Position.Left], ['right', Position.Right], ['top', Position.Top], ['bottom', Position.Bottom]];
const hidden = { opacity: 0, pointerEvents: 'none', width: 1, height: 1, minWidth: 1, minHeight: 1, border: 0 };

function CompNode({ data }) {
  const k = KIND[data.kind] ?? KIND.config;
  return (
    <div style={{
      width: data.w, height: data.h, boxSizing: 'border-box',
      background: 'var(--panel)', color: 'var(--text)',
      border: `1.6px solid ${k.c}`,
      borderLeft: data.kind === 'table' ? `6px solid ${k.c}` : undefined,
      borderRadius: 10, padding: '6px 10px', textAlign: 'left',
    }}>
      {SIDES.map(([n, p]) => <Handle key={'s' + n} id={'s-' + n} type="source" position={p} style={hidden} />)}
      {SIDES.map(([n, p]) => <Handle key={'t' + n} id={'t-' + n} type="target" position={p} style={hidden} />)}
      <div style={{ color: k.c, font: `700 10px ${MONO}`, letterSpacing: '0.05em' }}>{k.g} {data.label}</div>
      {data.sub && <div style={{ color: 'var(--dim)', font: '9px sans-serif', marginTop: 2 }}>{data.sub}</div>}
    </div>
  );
}

function GroupNode({ data }) {
  return (
    <div style={{
      width: '100%', height: '100%', position: 'relative',
      background: `${data.color}0c`, border: `1px dashed ${data.color}55`, borderRadius: 14,
    }}>
      {SIDES.map(([n, p]) => <Handle key={'s' + n} id={'s-' + n} type="source" position={p} style={hidden} />)}
      {SIDES.map(([n, p]) => <Handle key={'t' + n} id={'t-' + n} type="target" position={p} style={hidden} />)}
      <div style={{
        position: 'absolute', top: -11, left: 8, font: `700 10px ${MONO}`, letterSpacing: '0.08em',
        color: data.color, textTransform: 'uppercase', whiteSpace: 'nowrap',
        background: 'var(--bg)', padding: '2px 8px', borderRadius: 6, border: `1px solid ${data.color}66`,
      }}>{data.label}</div>
    </div>
  );
}

function RoutedEdge({ id, data, markerEnd, style }) {
  return (
    <>
      <BaseEdge id={id} path={data.path} markerEnd={markerEnd} style={style} />
      {data.label && (
        <EdgeLabelRenderer>
          <div className="nodrag nopan" style={{
            position: 'absolute',
            transform: `translate(-50%,-50%) translate(${data.labelPos.x}px,${data.labelPos.y}px)`,
            background: 'var(--bg)', border: '1px solid var(--border)', borderRadius: 6,
            padding: '1px 6px', font: `9px/1.4 ${MONO}`, color: 'var(--muted)',
            pointerEvents: 'none', whiteSpace: 'nowrap',
          }}>{data.label}</div>
        </EdgeLabelRenderer>
      )}
    </>
  );
}

const nodeTypes = { comp: CompNode, grouprect: GroupNode };
const edgeTypes = { routed: RoutedEdge };

function pickSides(a, b) {
  const dx = (b.x + b.w / 2) - (a.x + a.w / 2);
  const dy = (b.y + b.h / 2) - (a.y + a.h / 2);
  if (Math.abs(dx) >= Math.abs(dy)) return dx >= 0 ? ['right', 'left'] : ['left', 'right'];
  return dy >= 0 ? ['bottom', 'top'] : ['top', 'bottom'];
}
function anchor(r, side, frac = 0) {
  if (side === 'left') return { x: r.x, y: r.y + r.h * (0.5 + frac) };
  if (side === 'right') return { x: r.x + r.w, y: r.y + r.h * (0.5 + frac) };
  if (side === 'top') return { x: r.x + r.w * (0.5 + frac), y: r.y };
  return { x: r.x + r.w * (0.5 + frac), y: r.y + r.h };
}
const manhattan = (a, b) => Math.abs(a.x - b.x) + Math.abs(a.y - b.y);
function elbow(a, b, ss, ts) {
  const sh = ss === 'left' || ss === 'right';
  const th = ts === 'left' || ts === 'right';
  if (sh && th) { const mx = (a.x + b.x) / 2; return [a, { x: mx, y: a.y }, { x: mx, y: b.y }, b]; }
  if (!sh && !th) { const my = (a.y + b.y) / 2; return [a, { x: a.x, y: my }, { x: b.x, y: my }, b]; }
  if (sh && !th) return [a, { x: b.x, y: a.y }, b];
  return [a, { x: a.x, y: b.y }, b];
}
function routeWithRetries(a, b, obstacles, ss, ts, occupied) {
  const base = { sourceDir: ss, targetDir: ts, occupied };
  const pts =
    routeOrthogonal(a.x, a.y, b.x, b.y, obstacles, base) ||
    routeOrthogonal(a.x, a.y, b.x, b.y, obstacles, { ...base, clear: 8, stub: 12 }) ||
    routeOrthogonal(a.x, a.y, b.x, b.y, obstacles, { ...base, clear: 2, stub: 6 });
  return pts ? simplify(pts) : elbow(a, b, ss, ts);
}

function routeAll(nodeList, graph) {
  const rect = {};
  for (const n of nodeList) if (n.type === 'comp') rect[n.id] = { x: n.position.x, y: n.position.y, w: n.data.w, h: n.data.h };
  const geo = { ...rect };
  for (const n of nodeList) if (n.type === 'grouprect') geo[n.id] = { x: n.position.x, y: n.position.y, w: parseFloat(n.style.width), h: parseFloat(n.style.height) };

  const meta = graph.base
    .filter((e) => geo[e.source] && geo[e.target])
    .map((e) => { const [ss, ts] = pickSides(geo[e.source], geo[e.target]); return { e, ss, ts }; });

  const groups = {};
  meta.forEach((m, i) => {
    (groups[`${m.e.source}|${m.ss}`] ??= []).push(`${i}|s`);
    (groups[`${m.e.target}|${m.ts}`] ??= []).push(`${i}|t`);
  });
  const oppCoord = (key, axis) => {
    const [i, end] = key.split('|');
    const m = meta[+i];
    const r = end === 's' ? geo[m.e.target] : geo[m.e.source];
    return axis === 'x' ? r.x + r.w / 2 : r.y + r.h / 2;
  };
  const frac = {};
  for (const k in groups) {
    const side = k.split('|')[1];
    const axis = side === 'left' || side === 'right' ? 'y' : 'x';
    const g = groups[k].slice().sort((a, b) => oppCoord(a, axis) - oppCoord(b, axis));
    const n = g.length;
    g.forEach((key, j) => { frac[key] = n === 1 ? 0 : (j / (n - 1) - 0.5) * 0.6; });
  }
  const srcA = (i) => anchor(geo[meta[i].e.source], meta[i].ss, frac[`${i}|s`]);
  const tgtA = (i) => anchor(geo[meta[i].e.target], meta[i].ts, frac[`${i}|t`]);
  const order = meta.map((_, i) => i).sort((a, b) => manhattan(srcA(b), tgtA(b)) - manhattan(srcA(a), tgtA(a)));

  const occupied = [];
  const pathsById = {};
  for (const i of order) {
    const { e } = meta[i];
    const a = srcA(i), b = tgtA(i);
    const obstacles = Object.entries(rect).filter(([id]) => id !== e.source && id !== e.target).map(([, r]) => r);
    const poly = routeWithRetries(a, b, obstacles, meta[i].ss, meta[i].ts, occupied);
    occupied.push(poly);
    pathsById[i] = poly;
  }

  return meta.map(({ e, ss, ts }, i) => ({
    id: `be${i}`, source: e.source, target: e.target,
    sourceHandle: `s-${ss}`, targetHandle: `t-${ts}`,
    type: 'routed',
    data: { path: roundedPath(pathsById[i]), label: e.label, labelPos: polylineMidpoint(pathsById[i]) },
    markerEnd: { type: MarkerType.ArrowClosed, color: EDGE, width: 13, height: 13 },
    style: { stroke: EDGE, strokeWidth: 1.2, ...(e.dashed ? { strokeDasharray: '7 5' } : {}) },
  }));
}

const THEME = {
  '--bg': '#0D1117', '--panel': '#161B22', '--border': '#30363D', '--text': '#E6EDF3',
  '--muted': '#9DA7B3', '--dim': '#6E7681', '--green': '#3FB950',
};

// dark-theme the zoom controls (the RF stylesheet styles them light by default)
const CONTROLS_CSS = `
.fleet-graph .react-flow__controls-button { background: var(--panel); border-bottom: 1px solid var(--border); fill: var(--text); }
.fleet-graph .react-flow__controls-button:hover { background: var(--border); }
`;

export default function FleetGraph({ graph, padding = 0.12, interactive = true }) {
  const nodes = useMemo(() => {
    const groupNodes = (graph.groups ?? []).map((grp) => ({
      id: `grp-${grp.id}`, type: 'grouprect',
      position: { x: grp.x * SPREAD, y: grp.y * SPREAD },
      data: { label: grp.label, color: grp.color },
      style: { width: grp.w * SPREAD, height: grp.h * SPREAD },
      selectable: false, draggable: false, zIndex: 0,
    }));
    const compNodes = graph.nodes.map((n) => ({
      id: n.id, type: 'comp', position: { x: n.x * SPREAD, y: n.y * SPREAD },
      data: { kind: n.kind, label: n.label, sub: n.sub, w: n.w, h: n.h }, zIndex: 1,
    }));
    return [...groupNodes, ...compNodes];
  }, [graph]);

  const edges = useMemo(() => routeAll(nodes, graph), [nodes, graph]);

  return (
    <div className="fleet-graph" style={{ width: '100%', height: '100%', ...THEME, background: 'var(--bg)' }}>
      <style>{CONTROLS_CSS}</style>
      <ReactFlow
        nodes={nodes} edges={edges}
        nodeTypes={nodeTypes} edgeTypes={edgeTypes}
        fitView fitViewOptions={{ padding }}
        minZoom={0.05} maxZoom={4}
        nodesDraggable={false} nodesConnectable={false} elementsSelectable={false}
        panOnDrag={interactive} zoomOnScroll={interactive} zoomOnPinch={interactive} zoomOnDoubleClick={interactive}
        preventScrolling={interactive}
        proOptions={{ hideAttribution: true }}
      >
        <Background color="#30363D" gap={22} size={1} />
        {interactive && <Controls showInteractive={false} />}
      </ReactFlow>
    </div>
  );
}
