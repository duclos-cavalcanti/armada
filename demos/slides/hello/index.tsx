import {
  type DesignSystem,
  MorphElement,
  type Page,
  type SlideMeta,
  type SlideTransition,
  Step,
  Steps,
} from '@open-slide/core';
import { Background, Controls, type Edge, type Node, ReactFlow } from '@xyflow/react';
import '@xyflow/react/dist/style.css';
import type { CSSProperties } from 'react';
// Slide-local asset — Vite resolves an SVG import to its URL (open-slide assets rule).
import domainsBirdseye from './assets/domains-birdseye.svg';

// Per-slide design system (open-slide's theming hook). A calm dark palette so
// the hello deck feels on-brand next to the factory's mady-style surfaces.
export const design: DesignSystem = {
  palette: { bg: '#0D1117', text: '#E6EDF3', accent: '#58A6FF' },
  fonts: {
    display: '-apple-system, BlinkMacSystemFont, "Inter", system-ui, sans-serif',
    body: '-apple-system, BlinkMacSystemFont, "Inter", system-ui, sans-serif',
  },
  typeScale: { hero: 120, body: 30 },
  radius: 12,
};

const stage: CSSProperties = {
  width: '100%',
  height: '100%',
  position: 'relative',
  background: '#0D1117',
  overflow: 'hidden',
  color: '#E6EDF3',
  fontFamily: 'var(--osd-font-body)',
};

// Morph transition: held exit + opacity-only enter keep all motion on the
// morphing clones (per the slide-authoring morph rules).
const morph: SlideTransition = {
  duration: 280,
  exit: { duration: 300, easing: 'cubic-bezier(0.4, 0, 1, 1)', keyframes: [{ opacity: 1 }, { opacity: 1 }] },
  enter: { duration: 300, easing: 'cubic-bezier(0, 0, 0.2, 1)', keyframes: [{ opacity: 0 }, { opacity: 1 }] },
  morph: { duration: 760, easing: 'cubic-bezier(0.4, 0, 0.2, 1)' },
};

// ── Page 1 — cover. A small accent chip sits bottom-left; it morphs on cut. ──
const Cover: Page = () => (
  <div style={stage}>
    <div style={{ position: 'absolute', left: 160, top: 360 }}>
      <div style={{ fontSize: 120, fontWeight: 700, fontFamily: 'var(--osd-font-display)' }}>Hello, MADY</div>
      <div style={{ fontSize: 30, color: '#9DA7B3', marginTop: 16 }}>open-slide · factory demo workspace</div>
    </div>
    <MorphElement id="chip">
      <div style={{ position: 'absolute', left: 160, top: 640, width: 220, height: 64, borderRadius: 999, background: '#58A6FF' }} />
    </MorphElement>
  </div>
);

// ── Page 2 — the same chip morphs into a wide banner; bullets reveal in time. ──
const Reveal: Page = () => (
  <div style={stage}>
    <MorphElement id="chip">
      <div style={{ position: 'absolute', left: 160, top: 180, width: 1600, height: 64, borderRadius: 16, background: '#1F9E6E' }} />
    </MorphElement>
    <div style={{ position: 'absolute', left: 160, top: 320, fontSize: 44, fontWeight: 600 }}>Why open-slide here</div>
    <Steps>
      <Step><div style={{ position: 'absolute', left: 160, top: 420, fontSize: 30 }}>◆ Every slide is a React component → react-flow embeds natively (one world).</div></Step>
      <Step><div style={{ position: 'absolute', left: 160, top: 482, fontSize: 30 }}>◆ MorphElement morphs the diagram as the system evolves (actors i → ii → iii).</div></Step>
      <Step><div style={{ position: 'absolute', left: 160, top: 544, fontSize: 30 }}>◆ Source tracked in demos/; dist + node_modules gitignored.</div></Step>
    </Steps>
  </div>
);

// ── Page 3 — a live React Flow canvas, embedded as a plain component. ──
const card = (color: string): CSSProperties => ({
  background: '#161B22',
  color: '#E6EDF3',
  border: `1.5px solid ${color}`,
  borderRadius: 8,
  padding: '8px 12px',
  fontSize: 13,
  fontFamily: 'ui-monospace, Menlo, monospace',
  width: 210,
});

const rfNodes: Node[] = [
  { id: 'post', position: { x: 0, y: 40 }, data: { label: 'POST /v1/actors' }, style: card('#58A6FF') },
  { id: 'onboard', position: { x: 290, y: 40 }, data: { label: 'onboard_actor (λ)' }, style: card('#D29922') },
  { id: 'anchor', position: { x: 580, y: 40 }, data: { label: 'blank anchor — zero scope' }, style: card('#6E7681') },
  { id: 'attach', position: { x: 580, y: 190 }, data: { label: 'POST /v1/domains/{D}/actors' }, style: card('#58A6FF') },
  { id: 'grant', position: { x: 870, y: 190 }, data: { label: 'casbin grant → reach in D' }, style: card('#3FB950') },
];

const rfEdges: Edge[] = [
  { id: 'e1', source: 'post', target: 'onboard', animated: true, label: 'create {roles}' },
  { id: 'e2', source: 'onboard', target: 'anchor' },
  { id: 'e3', source: 'anchor', target: 'attach', label: 'later: attach to D' },
  { id: 'e4', source: 'attach', target: 'grant', animated: true, label: 'activation' },
];

const Graph: Page = () => (
  <div style={stage}>
    <div style={{ position: 'absolute', left: 160, top: 96, fontSize: 44, fontWeight: 600 }}>React-Flow</div>
    <div style={{ position: 'absolute', left: 160, top: 156, fontSize: 24, color: '#9DA7B3' }}>
      a live React Flow canvas is just a component in the page — no iframe
    </div>
    <div style={{ position: 'absolute', left: 160, top: 230, width: 1600, height: 740, border: '1px solid #30363D', borderRadius: 12, overflow: 'hidden' }}>
      <ReactFlow nodes={rfNodes} edges={rfEdges} fitView minZoom={0.3}>
        <Background color="#30363D" gap={22} size={1} />
        <Controls showInteractive={false} />
      </ReactFlow>
    </div>
  </div>
);

// ── Page 4 — embed an existing factory SVG (mady-diagram/flowchart output). ──
const Svg: Page = () => (
  <div style={stage}>
    <div style={{ position: 'absolute', left: 160, top: 96, fontSize: 44, fontWeight: 600 }}>SVGs</div>
    <div style={{ position: 'absolute', left: 160, top: 156, fontSize: 24, color: '#9DA7B3' }}>
      import the .svg asset → render as &lt;img&gt; (any mady-diagram / mady-flowchart output drops straight in)
    </div>
    <img
      src={domainsBirdseye}
      alt="domains birds-eye"
      style={{ position: 'absolute', left: 160, top: 230, width: 1600, height: 740, objectFit: 'contain' }}
    />
  </div>
);

// Transition on the incoming page; assign to all so morphs play stepping back too.
Cover.transition = morph;
Reveal.transition = morph;
Graph.transition = morph;
Svg.transition = morph;

export const meta: SlideMeta = { title: 'Hello, MADY' };
export default [Cover, Reveal, Graph, Svg] satisfies Page[];
