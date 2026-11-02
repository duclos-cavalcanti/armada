import { type DesignSystem, type Page, type SlideMeta, Steps, Step as Beat } from '@open-slide/core';
import type { CSSProperties, ReactNode } from 'react';
import FleetGraph from './fleet/FleetGraph';
import overviewGraph from './fleet/overview.json';
import dataGraph from './fleet/data.json';

export const design: DesignSystem = {
  palette: { bg: '#0b0b0d', text: '#e8eaed', accent: '#4aa3ff' },
  fonts: {
    display: '-apple-system, BlinkMacSystemFont, "Inter", system-ui, sans-serif',
    body: '-apple-system, BlinkMacSystemFont, "Inter", system-ui, sans-serif',
  },
  typeScale: { hero: 128, body: 30 },
  radius: 12,
};

// ─── palette ──────────────────────────────────────────────────────────────
// Node colors mirror the KIND table in the fleet viewer, so the graph slides
// and the hand-built slides read as one system.
const C = {
  bg: '#0b0b0d', text: '#e8eaed', accent: '#4aa3ff', dim: '#9aa0a6',
  panel: '#15161a', panel2: '#1b1d22', border: '#2a2b31',
  human: '#58A6FF', skill: '#D2A8FF', cog: '#3FB950', cli: '#79C0FF',
  daemon: '#BC8CFF', expert: '#D2A8FF', external: '#DB6D28', sqlite: '#56D364',
  adapter: '#D29922', dto: '#39C5CF', warn: '#F0883E',
};
const MONO = 'ui-monospace, SFMono-Regular, Menlo, monospace';

const stage: CSSProperties = {
  width: '100%', height: '100%', position: 'relative',
  background: C.bg, overflow: 'hidden', color: C.text,
  fontFamily: 'var(--osd-font-body)',
};

// ─── shared pieces ──────────────────────────────────────────────────────────
function Title({ children, kicker }: { children: ReactNode; kicker?: string }) {
  return (
    <div style={{ position: 'absolute', left: 80, top: 46 }}>
      {kicker && (
        <div style={{ font: `700 18px ${MONO}`, letterSpacing: '0.18em', color: C.accent, textTransform: 'uppercase', marginBottom: 8 }}>
          {kicker}
        </div>
      )}
      <div style={{ fontSize: 50, fontWeight: 600, letterSpacing: '-0.01em' }}>{children}</div>
    </div>
  );
}

// a titled panel
function Pane({ accent, head, note, style, children }: {
  accent: string; head: string; note?: string; style?: CSSProperties; children?: ReactNode;
}) {
  return (
    <div style={{
      background: C.panel, border: `1px solid ${C.border}`, borderTop: `3px solid ${accent}`,
      borderRadius: 14, padding: '22px 26px', ...style,
    }}>
      <div style={{ font: `700 15px ${MONO}`, letterSpacing: '0.04em', color: accent }}>{head}</div>
      {note && <div style={{ fontSize: 18, color: C.dim, marginTop: 6, lineHeight: 1.35 }}>{note}</div>}
      {children}
    </div>
  );
}

// one labelled row: glyph chip + name + one-line role
function Row({ color, glyph, name, role }: { color: string; glyph: string; name: string; role: string }) {
  return (
    <div style={{ display: 'flex', alignItems: 'baseline', gap: 14, padding: '7px 0' }}>
      <span style={{
        flex: '0 0 auto', width: 170, font: `700 18px ${MONO}`, color,
        borderLeft: `4px solid ${color}`, paddingLeft: 12,
      }}>{glyph} {name}</span>
      <span style={{ fontSize: 18, color: C.text, lineHeight: 1.35 }}>{role}</span>
    </div>
  );
}

// ─── 1 · cover ──────────────────────────────────────────────────────────────
const Cover: Page = () => (
  <div style={stage}>
    <div style={{ position: 'absolute', left: 120, top: 380 }}>
      <div style={{ font: `700 22px ${MONO}`, letterSpacing: '0.2em', color: C.accent, textTransform: 'uppercase' }}>Armada</div>
      <div style={{ fontSize: 132, fontWeight: 700, fontFamily: 'var(--osd-font-display)', letterSpacing: '-0.03em', marginTop: 10 }}>Factory</div>
      <div style={{ fontSize: 40, fontWeight: 500, color: C.dim, marginTop: 8 }}>Agentic Software Factory</div>
      <div style={{ fontSize: 26, color: C.dim, marginTop: 40 }}>Daniel Duclos-Cavalcanti — SWE II</div>
    </div>
  </div>
);

// ─── 2 · getting started (process → skill → script → agent → loop) ──────────
// Card geometry: four equal columns across the 1760px content width.
const GS_W = 398;
const GS_GAP = 56;
const gsLeft = (i: number) => 80 + i * (GS_W + GS_GAP);

// One stage of the build-up: numbered head, title, a visual, a caption.
function GsCard({ i, color, head, title, caption, children }: {
  i: number; color: string; head: string; title: string; caption: ReactNode; children: ReactNode;
}) {
  return (
    <div style={{
      position: 'absolute', left: gsLeft(i), top: 190, width: GS_W, height: 540, boxSizing: 'border-box',
      background: C.panel, border: `1px solid ${C.border}`, borderTop: `3px solid ${color}`, borderRadius: 14,
      padding: '22px 24px', display: 'flex', flexDirection: 'column',
    }}>
      <div style={{ font: `700 15px ${MONO}`, letterSpacing: '0.08em', color, textTransform: 'uppercase' }}>{head}</div>
      <div style={{ fontSize: 28, fontWeight: 600, marginTop: 8 }}>{title}</div>
      <div style={{ marginTop: 18 }}>{children}</div>
      <div style={{ marginTop: 'auto', fontSize: 19, color: C.dim, lineHeight: 1.4, fontWeight: '700' }}>{caption}</div>
    </div>
  );
}

// The arrow that lands in the gap left of card i.
function GsArrow({ i }: { i: number }) {
  return (
    <div style={{ position: 'absolute', left: gsLeft(i) - GS_GAP, top: 430, width: GS_GAP, textAlign: 'center', fontSize: 36, color: C.dim }}>→</div>
  );
}

// A file excerpt: path bar + monospace body.
function GsFile({ path, color, children }: { path: string; color: string; children: ReactNode }) {
  return (
    <div style={{ background: C.panel2, border: `1px solid ${C.border}`, borderRadius: 10, overflow: 'hidden' }}>
      <div style={{ font: `600 13px ${MONO}`, color, padding: '7px 12px', borderBottom: `1px solid ${C.border}` }}>{path}</div>
      <pre style={{ margin: 0, padding: '10px 12px', font: `400 15px/1.55 ${MONO}`, color: C.text, whiteSpace: 'pre' }}>{children}</pre>
    </div>
  );
}

// A CI job chip; a failed one pulses.
function GsJob({ name, state }: { name: string; state: 'ok' | 'fail' | 'wait' }) {
  const col = state === 'ok' ? C.cog : state === 'fail' ? C.warn : C.dim;
  const glyph = state === 'ok' ? '✓' : state === 'fail' ? '✗' : '⏸';
  return (
    <div className={state === 'fail' ? 'gs-pulse' : undefined} style={{
      font: `600 15px ${MONO}`, color: col, border: `1px solid ${col}66`, background: `${col}14`,
      borderRadius: 8, padding: '6px 10px', marginTop: 8,
    }}>{glyph} {name}</div>
  );
}
function GsStage({ name, children }: { name: string; children: ReactNode }) {
  return (
    <div style={{ flex: 1 }}>
      <div style={{ font: `700 13px ${MONO}`, color: C.dim, letterSpacing: '0.06em', textTransform: 'uppercase' }}>{name}</div>
      {children}
    </div>
  );
}

// Relative cost of one status check, illustrative only.
function GsBar({ label, width, color }: { label: string; width: number; color: string }) {
  return (
    <div style={{ marginTop: 10 }}>
      <div style={{ font: `500 13px ${MONO}`, color: C.dim }}>{label}</div>
      <div style={{ height: 12, width, background: color, borderRadius: 6, marginTop: 4 }} />
    </div>
  );
}

// The live loop: four phases on a ring with a token circling them.
function GsRing() {
  const ring = 'M 300 22 A 250 78 0 1 1 299.9 22 Z';
  const phase = (x: number, y: number, t: string, c: string = C.text) => (
    <g key={t}>
      <rect x={x - 92} y={y - 17} width={184} height={34} rx={17} fill={C.panel2} stroke={c} strokeWidth={1.5} />
      <text x={x} y={y + 5} textAnchor="middle" fill={c} fontFamily={MONO} fontSize={15} fontWeight={700}>{t}</text>
    </g>
  );
  return (
    <svg viewBox="0 0 600 200" style={{ width: 600, height: 200 }}>
      <path d={ring} fill="none" stroke="#3a3f47" strokeWidth={2} strokeDasharray="6 6" />
      <circle r={8} fill={C.accent}>
        <animateMotion dur="5s" repeatCount="indefinite" path={ring} />
      </circle>
      {phase(300, 22, 'Actuate via Skill', C.cli)}
      {phase(500, 100, 'Evaluate Output', C.cog)}
      {phase(300, 178, 'React', C.warn)}
      {phase(100, 100, 'State of the World', C.skill)}
    </svg>
  );
}

const GettingStarted: Page = () => (
  <div style={stage}>
    <style>{`
      @keyframes gs-pulse { 0%, 100% { box-shadow: 0 0 0 0 ${C.warn}00; } 50% { box-shadow: 0 0 0 6px ${C.warn}40; } }
      .gs-pulse { animation: gs-pulse 1.6s ease-in-out infinite; }
    `}</style>
    <Title kicker="getting started">Process → Loop</Title>
    <Steps>
      <Beat duration={400}>
        <GsCard i={0} color={C.external} head="① a process" title="Pipeline Diagnosis"
          caption="Human manual failure diagnosis">
          <div style={{ font: `600 14px ${MONO}`, color: C.dim }}>MR !123 · pipeline #8841</div>
          <div style={{ display: 'flex', gap: 10, marginTop: 12 }}>
            <GsStage name="build">
              <GsJob name="compile" state="ok" />
              <GsJob name="lint" state="ok" />
            </GsStage>
            <GsStage name="test">
              <GsJob name="unit" state="ok" />
              <GsJob name="e2e" state="fail" />
            </GsStage>
            <GsStage name="deploy">
              <GsJob name="staging" state="wait" />
            </GsStage>
          </div>
        </GsCard>
      </Beat>
      <Beat duration={400}>
        <div>
          <GsArrow i={1} />
          <GsCard i={1} color={C.skill} head="② a skill" title="Encapsulating Logic"
            caption="Plain text: Describe what to do and how to classify/react to output.">
            <GsFile path="skills/pipeline-status/SKILL.md" color={C.skill}>{`---
name: pipeline-status
argument: <mr>
---
Find the MR's running pipeline.
For each stage, each job:
report its state ✓ ✗ ⏳ ⏸
and summarise per stage.`}</GsFile>
          </GsCard>
        </div>
      </Beat>
      <Beat duration={400}>
        <div>
          <GsArrow i={2} />
          <GsCard i={2} color={C.cli} head="③ + a script" title="Mechanical/Deterministic"
            caption="Repeatable, deterministic logic.">
            <GsFile path="skills/pipeline-status/" color={C.cli}>{`├─ SKILL.md
└─ scripts/
   └─ status.sh  `}<span style={{ color: C.cli }}>{`← glab ci`}</span></GsFile>
            <div style={{ marginTop: 14, font: `700 13px ${MONO}`, color: C.dim, letterSpacing: '0.06em', textTransform: 'uppercase' }}>tokens per check (illustrative)</div>
            <GsBar label="skill alone — model queries the API" width={340} color={`${C.warn}aa`} />
            <GsBar label="skill + status.sh" width={70} color={C.cli} />
          </GsCard>
        </div>
      </Beat>
      <Beat duration={400}>
        <div>
          <GsArrow i={3} />
          <GsCard i={3} color={C.cog} head="④ an agent" title="Process with Agency"
            caption={<>
              What is its goal<br />
              What it knows<br />
              What it's allowed to do<br />
              How to correlate output and react
            </>}>
            <GsFile path="agents/pipeline-medic.md" color={C.cog}>{`---
skills: [pipeline-status]
initialPrompt: >
  /loop /pipeline-status !123
---
Knows: repo, .gitlab-ci.yml
Consults: docs/ci-failures.md
May:  read logs, retry jobs
Must: diagnose every ✗ job`}</GsFile>
          </GsCard>
        </div>
      </Beat>
      <Beat duration={500}>
        <div style={{
          position: 'absolute', left: 80, right: 80, top: 770, height: 240, boxSizing: 'border-box',
          background: C.panel, border: `1px solid ${C.border}`, borderTop: `3px solid ${C.accent}`, borderRadius: 14,
          padding: '18px 28px', display: 'flex', alignItems: 'center', gap: 40,
        }}>
          <div style={{ width: 470 }}>
            <div style={{ font: `700 15px ${MONO}`, letterSpacing: '0.08em', color: C.accent, textTransform: 'uppercase' }}>⑤ the loop</div>
            <div style={{ fontSize: 28, fontWeight: 600, marginTop: 8 }}>Algorithm</div>
            <ul style={{ fontSize: 20, lineHeight: 1.55, margin: '10px 0 0', paddingLeft: 22 }}>
              <li>a <b>loop</b> and a <b>goal,</b>{''}</li>
              <li><b>accepted states</b>{' of its world,'}</li>
              <li>{'and a break condition '}<b>{''}</b>{''}</li>
            </ul>
          </div>
          <GsRing />
          <div style={{ flex: 1 }}>
            <div style={{ font: `700 15px ${MONO}`, color: C.human }}>threshold crossed?</div>
            <div style={{ fontSize: 24, fontWeight: 600, marginTop: 6 }}>→ Escalate for judgment</div>
            <div style={{ fontSize: 19, color: C.dim, marginTop: 14, lineHeight: 1.4 }}>
              {''}<br />{''}
            </div>
          </div>
        </div>
      </Beat>
    </Steps>
  </div>
);

// ─── sdlc (the line + escalation triage) ─────────────────────────────────────
function SdlcDiagram() {
  // Main row: six stages across the 1760px width.
  const W = 236, H = 116, Y = 150;
  const GAP = (1760 - 6 * W) / 5;
  const cx = (i: number) => W / 2 + i * (W + GAP);
  const stage = (i: number, label: string, sub: string, color: string) => (
    <g key={label}>
      <rect x={cx(i) - W / 2} y={Y - H / 2} width={W} height={H} rx={14} fill={C.panel2} stroke={color} strokeWidth={2.4} />
      <text x={cx(i)} y={Y - 4} textAnchor="middle" fill={color} fontSize={27} fontWeight={700}>{label}</text>
      <text x={cx(i)} y={Y + 28} textAnchor="middle" fill={C.dim} fontFamily={MONO} fontSize={16}>{sub}</text>
    </g>
  );
  const flow = (i: number) => (
    <line key={i} x1={cx(i) + W / 2 + 6} y1={Y} x2={cx(i + 1) - W / 2 - 8} y2={Y}
      stroke="#5a6470" strokeWidth={2.4} markerEnd="url(#sdlc-ah)" />
  );
  const lbl = (x: number, y: number, t: string, c: string = C.dim) => (
    <text x={x} y={y} textAnchor="middle" fill={c} fontFamily={MONO} fontSize={17}>{t}</text>
  );

  // Escalation: Implementation + Merge feed one TRIAGE decision.
  const IMPL = cx(3), MERGE = cx(4);
  const TX = (IMPL + MERGE) / 2, TY = 450, TW = 130, TH = 78;
  const RX = 1560, RW = 240, RH = 96;
  const BOTTOM = 690;
  return (
    <svg viewBox="0 0 1760 780" style={{ width: '100%', height: '100%' }}>
      <defs>
        <marker id="sdlc-ah" markerWidth="12" markerHeight="12" refX="9" refY="5" orient="auto">
          <path d="M0,0 L10,5 L0,10 z" fill="#5a6470" />
        </marker>
        <marker id="sdlc-ah-warn" markerWidth="12" markerHeight="12" refX="9" refY="5" orient="auto">
          <path d="M0,0 L10,5 L0,10 z" fill={C.warn} />
        </marker>
        <marker id="sdlc-ah-ok" markerWidth="12" markerHeight="12" refX="9" refY="5" orient="auto">
          <path d="M0,0 L10,5 L0,10 z" fill={C.cog} />
        </marker>
        <marker id="sdlc-ah-human" markerWidth="12" markerHeight="12" refX="9" refY="5" orient="auto">
          <path d="M0,0 L10,5 L0,10 z" fill={C.human} />
        </marker>
      </defs>

      {/* the line */}
      {flow(0)}{flow(1)}{flow(2)}{flow(3)}{flow(4)}
      {stage(0, 'Operator', 'you', C.human)}
      {stage(1, 'Tickets', 'admiral · Ready', C.external)}
      {stage(2, 'Dispatch', 'captain', C.daemon)}
      {stage(3, 'Implementation', 'worker · hand', C.cog)}
      {stage(4, 'Merge', 'sailor', C.dto)}
      {stage(5, 'Done', 'reconcile reaps', C.sqlite)}

      {/* the gates: DoR feeds Tickets, DoD guards Done */}
      <line x1={cx(1)} y1={312} x2={cx(1)} y2={Y + H / 2 + 14} stroke="#5a6470" strokeWidth={2} markerEnd="url(#sdlc-ah)" />
      {lbl(cx(1), 340, 'Definition of Ready')}
      <line x1={cx(5)} y1={312} x2={cx(5)} y2={Y + H / 2 + 14} stroke="#5a6470" strokeWidth={2} markerEnd="url(#sdlc-ah)" />
      {lbl(cx(5), 340, 'Definition of Done')}

      {/* escalate → TRIAGE */}
      <path d={`M ${IMPL} ${Y + H / 2 + 4} L ${TX - 24} ${TY - TH - 8}`} fill="none" stroke={C.warn} strokeWidth={2.2} strokeDasharray="8 7" markerEnd="url(#sdlc-ah-warn)" />
      <path d={`M ${MERGE} ${Y + H / 2 + 4} L ${TX + 24} ${TY - TH - 8}`} fill="none" stroke={C.warn} strokeWidth={2.2} strokeDasharray="8 7" markerEnd="url(#sdlc-ah-warn)" />
      {lbl(TX, 300, 'escalate', C.warn)}
      <polygon points={`${TX},${TY - TH} ${TX + TW},${TY} ${TX},${TY + TH} ${TX - TW},${TY}`} fill={C.panel2} stroke={C.warn} strokeWidth={2.4} />
      <text x={TX} y={TY - 8} textAnchor="middle" fill={C.warn} fontFamily={MONO} fontSize={26} fontWeight={700} letterSpacing="0.1em">TRIAGE</text>
      <text x={TX} y={TY + 20} textAnchor="middle" fill={C.dim} fontFamily={MONO} fontSize={16}>captain</text>
      <text x={TX} y={TY + 44} textAnchor="middle" fill={C.dim} fontFamily={MONO} fontSize={14} fontStyle="italic">fixable?</text>

      {/* fixable → resolve */}
      <line x1={TX + TW + 6} y1={TY} x2={RX - RW / 2 - 8} y2={TY} stroke={C.cog} strokeWidth={2.4} markerEnd="url(#sdlc-ah-ok)" />
      {lbl((TX + TW + RX - RW / 2) / 2, TY - 14, 'yes', C.cog)}
      <rect x={RX - RW / 2} y={TY - RH / 2} width={RW} height={RH} rx={14} fill={C.panel2} stroke={C.cog} strokeWidth={2.4} />
      <text x={RX} y={TY - 4} textAnchor="middle" fill={C.cog} fontSize={27} fontWeight={700}>Resolve</text>
      <text x={RX} y={TY + 26} textAnchor="middle" fill={C.dim} fontFamily={MONO} fontSize={16}>cog fixes · resumes</text>

      {/* not fixable → back to the operator */}
      <path d={`M ${TX} ${TY + TH + 4} L ${TX} ${BOTTOM} L ${cx(0)} ${BOTTOM} L ${cx(0)} ${Y + H / 2 + 10}`}
        fill="none" stroke={C.human} strokeWidth={2.4} strokeDasharray="8 7" markerEnd="url(#sdlc-ah-human)" />
      {lbl(TX + 26, TY + TH + 50, 'no', C.human)}
      {lbl((TX + cx(0)) / 2, BOTTOM - 16, 'needs a human decision', C.human)}
    </svg>
  );
}

const Sdlc: Page = () => (
  <div style={stage}>
    <Title kicker="the line">SDLC</Title>
    <div style={{ position: 'absolute', left: 80, top: 200, width: 1760, height: 780 }}>
      <SdlcDiagram />
    </div>
  </div>
);

// ─── sdlc phases (one state machine per stage) ──────────────────────────────
// Shared SVG pieces for the per-phase pages. Arrowheads are drawn as polygons
// (no <marker>) so thumbnails and the live page never fight over marker ids.
type Pt = [number, number];
const SM_EDGE = '#5a6470';

function SmCanvas({ children }: { children: ReactNode }) {
  return (
    <div style={{ position: 'absolute', left: 80, top: 200, width: 1760, height: 780 }}>
      <svg viewBox="0 0 1760 780" style={{ width: '100%', height: '100%' }}>{children}</svg>
    </div>
  );
}

function SmNode({ x, y, label, sub, color, w = 300, h = 116 }: {
  x: number; y: number; label: string; sub?: string; color: string; w?: number; h?: number;
}) {
  return (
    <g>
      <rect x={x - w / 2} y={y - h / 2} width={w} height={h} rx={14} fill={C.panel2} stroke={color} strokeWidth={2.4} />
      <text x={x} y={sub ? y - 4 : y + 9} textAnchor="middle" fill={color} fontSize={27} fontWeight={700}>{label}</text>
      {sub && <text x={x} y={y + 28} textAnchor="middle" fill={C.dim} fontFamily={MONO} fontSize={16}>{sub}</text>}
    </g>
  );
}

function SmDecision({ x, y, label, sub, color, w = 130, h = 78 }: {
  x: number; y: number; label: string; sub?: string; color: string; w?: number; h?: number;
}) {
  return (
    <g>
      <polygon points={`${x},${y - h} ${x + w},${y} ${x},${y + h} ${x - w},${y}`} fill={C.panel2} stroke={color} strokeWidth={2.4} />
      <text x={x} y={sub ? y - 2 : y + 8} textAnchor="middle" fill={color} fontFamily={MONO} fontSize={24} fontWeight={700}>{label}</text>
      {sub && <text x={x} y={y + 26} textAnchor="middle" fill={C.dim} fontFamily={MONO} fontSize={15}>{sub}</text>}
    </g>
  );
}

// A polyline through `pts` with an arrowhead on the last point.
function SmEdge({ pts, color = SM_EDGE, dashed, label, at, labelColor }: {
  pts: Pt[]; color?: string; dashed?: boolean; label?: string; at?: Pt; labelColor?: string;
}) {
  const [x1, y1] = pts[pts.length - 2];
  const [x2, y2] = pts[pts.length - 1];
  const len = Math.hypot(x2 - x1, y2 - y1) || 1;
  const ux = (x2 - x1) / len, uy = (y2 - y1) / len;
  const tip: Pt = [x2, y2];
  const base: Pt = [x2 - 15 * ux, y2 - 15 * uy];
  const head = `${tip[0]},${tip[1]} ${base[0] - 7 * uy},${base[1] + 7 * ux} ${base[0] + 7 * uy},${base[1] - 7 * ux}`;
  const line = [...pts.slice(0, -1), [x2 - 12 * ux, y2 - 12 * uy] as Pt].map((p) => p.join(',')).join(' ');
  return (
    <g>
      <polyline points={line} fill="none" stroke={color} strokeWidth={2.4} strokeDasharray={dashed ? '8 7' : undefined} />
      <polygon points={head} fill={color} />
      {label && at && (
        <text x={at[0]} y={at[1]} textAnchor="middle" fill={labelColor ?? (color === SM_EDGE ? C.dim : color)} fontFamily={MONO} fontSize={17}>{label}</text>
      )}
    </g>
  );
}

function SmNote({ x, y, children }: { x: number; y: number; children: ReactNode }) {
  return <text x={x} y={y} textAnchor="middle" fill={C.dim} fontFamily={MONO} fontSize={17}>{children}</text>;
}

// ── phase 1 · tickets ──
const PhaseTickets: Page = () => (
  <div style={stage}>
    <Title kicker="sdlc · 1 of 5">Tickets</Title>
    <SmCanvas>
      <SmEdge pts={[[410, 300], [730, 300]]} label="epics · stories · intent" at={[570, 282]} />
      <SmEdge pts={[[1030, 300], [1350, 300]]} label="Ready tickets + blocked-by" at={[1190, 282]} />
      <SmEdge pts={[[1500, 358], [1500, 560], [260, 560], [260, 358]]} color={C.human} dashed
        label="review · refine · prioritise" at={[880, 542]} />
      <SmNode x={260} y={300} label="Operator" sub="you" color={C.human} />
      <SmNode x={880} y={300} label="Agent" sub="admiral · /fleet-commission" color={C.skill} />
      <SmNode x={1500} y={300} label="Jira" sub="tracker · To Do" color={C.external} />
      <SmNote x={880} y={420}>Draft Tickets to DoR</SmNote>
    </SmCanvas>
  </div>
);

// ── phase 2 · dispatch ──
const PhaseDispatch: Page = () => (
  <div style={stage}>
    <Title kicker="sdlc · 2 of 5">Dispatch</Title>
    <SmCanvas>
      <SmEdge pts={[[450, 260], [586, 260]]} />
      <SmEdge pts={[[850, 260], [992, 260]]} label="yes" at={[921, 242]} color={C.cog} />
      <SmEdge pts={[[1260, 260], [1422, 260]]} />
      <SmEdge pts={[[720, 338], [720, 502]]} label="no" at={[742, 430]} color={C.warn} />
      <SmEdge pts={[[580, 560], [458, 560]]} color={C.warn} dashed label="re-queued" at={[519, 542]} />
      {/* the orchestrator reads the queue and writes back to it */}
      <SmEdge pts={[[260, 502], [260, 318]]} label="consults" at={[200, 416]} />
      <SmEdge pts={[[340, 318], [340, 502]]} label="mutates" at={[400, 416]} />
      <SmNode x={300} y={260} label="Orchestrator" sub="captain" color={C.daemon} />
      <SmDecision x={720} y={260} label="Ready?" sub="DoR" color={C.daemon} />
      <SmNode x={1130} y={260} w={260} label="Worktree" sub="/fleet-berth" color={C.cli} />
      <SmNode x={1560} y={260} w={260} label="Dispatched" sub="fleetc dispatch" color={C.cog} />
      <SmNode x={300} y={560} label="Queue" sub="convoy" color={C.skill} />
      <SmNode x={720} y={560} w={260} label="Hold" color={C.warn} />
    </SmCanvas>
  </div>
);

// ── phase 3 · implementation ──
const PhaseImplementation: Page = () => (
  <div style={stage}>
    <Title kicker="sdlc · 3 of 5">Implementation</Title>
    <SmCanvas>
      <SmEdge pts={[[290, 260], [400, 260]]} />
      <SmEdge pts={[[660, 260], [760, 260]]} />
      <SmEdge pts={[[1020, 260], [1130, 260]]} />
      <SmEdge pts={[[1390, 260], [1480, 260]]} label="clean" at={[1435, 242]} color={C.cog} />
      <SmEdge pts={[[1260, 338], [1260, 520], [530, 520], [530, 318]]} color={C.warn}
        label="Fixes Needed" at={[895, 502]} />
      <SmEdge pts={[[1325, 299], [1560, 538]]} color={C.warn} dashed
        label="Threshold Break" at={[1540, 420]} />
      <SmNode x={150} y={260} w={260} label="Worker" sub="or a hand · you code" color={C.cog} />
      <SmNode x={530} y={260} w={260} label="Implement" sub="code · tests · lint" color={C.text} />
      <SmNode x={890} y={260} w={260} label="Draft MR" sub="/fleet-launch --draft" color={C.cli} />
      <SmDecision x={1260} y={260} label="Review" sub="reviewer" color={C.expert} />
      <SmNode x={1610} y={260} w={260} label="Concluded" sub="→ handoff · In QA" color={C.cog} />
      <SmNode x={1610} y={600} w={260} label="Escalate" sub="→ TRIAGE" color={C.warn} />
    </SmCanvas>
  </div>
);

// ── phase 4 · merge ──
const PhaseMerge: Page = () => (
  <div style={stage}>
    <Title kicker="sdlc · 4 of 5">Merge</Title>
    <SmCanvas>
      <SmEdge pts={[[280, 300], [390, 300]]} />
      {/* decision → one action per snapshot finding */}
      <SmEdge pts={[[650, 300], [700, 300], [700, 90], [790, 90]]} />
      <SmEdge pts={[[700, 210], [790, 210]]} />
      <SmEdge pts={[[700, 330], [790, 330]]} />
      <SmEdge pts={[[700, 300], [700, 450], [790, 450]]} />
      {/* every action ends the sweep */}
      <SmEdge pts={[[1070, 90], [1130, 90], [1130, 270], [1190, 270]]} />
      <SmEdge pts={[[1070, 210], [1130, 210]]} />
      <SmEdge pts={[[1070, 330], [1130, 330]]} />
      <SmEdge pts={[[1070, 450], [1130, 450], [1130, 300], [1190, 300]]} />
      <SmEdge pts={[[1330, 227], [1330, 20], [150, 20], [150, 242]]} color={C.dto} dashed
        label="sleep 60s" at={[420, 44]} />
      <SmEdge pts={[[520, 378], [520, 542]]} color={C.warn} dashed label="Threshold Break" at={[430, 470]} />
      <SmEdge pts={[[930, 492], [930, 640], [1480, 640]]} color={C.cog}
        label="forge gates pass" at={[1200, 622]} />
      <SmNode x={150} y={300} w={260} label="Merge Driver" sub="sailor" color={C.dto} />
      <SmDecision x={520} y={300} label="Sweep" sub="snapshot?" color={C.dto} />
      <SmNode x={930} y={90} w={280} h={84} label="Rebase" sub="/fleet-rechart" color={C.skill} />
      <SmNode x={930} y={210} w={280} h={84} label="Review Replies" sub="fixes found" color={C.skill} />
      <SmNode x={930} y={330} w={280} h={84} label="CI Repair" sub="/fleet-repair" color={C.skill} />
      <SmNode x={930} y={450} w={280} h={84} label="Arm" sub="clean → auto-merge" color={C.cog} />
      <SmNode x={1330} y={285} w={260} label="Sleeping" sub="sweep done" color={C.dim} />
      <SmNode x={520} y={600} w={260} label="Escalate" sub="→ TRIAGE" color={C.warn} />
      <SmNode x={1610} y={640} w={260} label="Merged" sub="the forge merges" color={C.sqlite} />
    </SmCanvas>
  </div>
);

// ── phase 5 · done ──
const PhaseDone: Page = () => (
  <div style={stage}>
    <Title kicker="sdlc · 5 of 5">Done</Title>
    <SmCanvas>
      <SmEdge pts={[[290, 260], [400, 260]]} />
      <SmEdge pts={[[660, 260], [740, 260]]} label="Reap" at={[700, 242]} />
      <SmEdge pts={[[1040, 260], [1130, 260]]} />
      <SmEdge pts={[[1390, 260], [1480, 260]]} />
      <SmEdge pts={[[1610, 318], [1610, 560], [1050, 560]]} color={C.daemon} dashed
        label="captain's next pass" at={[1330, 542]} />
      <SmNode x={150} y={260} w={260} label="MR merged" sub="DoD met" color={C.sqlite} />
      <SmNode x={530} y={260} w={260} label="Reconcile" sub="fleetd · ~60s tick" color={C.daemon} />
      <SmNode x={890} y={260} label="Jira Done" sub="sessions end · worktree freed" color={C.external} />
      <SmNode x={1260} y={260} w={260} label="Ledger" sub="one CDC event" color={C.sqlite} />
      <SmNode x={1610} y={260} w={260} label="Dependents" sub="blockers cleared" color={C.cog} />
      <SmNode x={890} y={560} label="Dispatch" sub="next frontier" color={C.daemon} />
      <SmNote x={150} y={380}>AC · reviewed · CI green</SmNote>
    </SmCanvas>
  </div>
);

// ─── 2 · the sightline (live overview graph) ─────────────────────────────────
const Sightline: Page = () => (
  <div style={stage}>
    <Title kicker="Software factory">Architecture</Title>
    <div style={{ position: 'absolute', left: 60, top: 150, width: 1800, height: 870 }}>
      <FleetGraph graph={overviewGraph} />
    </div>
  </div>
);

// ─── 4 · two interfaces ───────────────────────────────────────────────────────
function VerbList({ items, color }: { items: [string, string][]; color: string }) {
  return (
    <div style={{ marginTop: 16 }}>
      {items.map(([v, d]) => (
        <div key={v} style={{ display: 'flex', alignItems: 'baseline', gap: 12, padding: '5px 0' }}>
          <code style={{ flex: '0 0 auto', width: 210, font: `700 17px ${MONO}`, color }}>{v}</code>
          <span style={{ fontSize: 17, color: C.text, lineHeight: 1.3 }}>{d}</span>
        </div>
      ))}
    </div>
  );
}
const TwoInterfaces: Page = () => (
  <div style={stage}>
    <Title kicker="how you drive it">Two interfaces, one surface</Title>
    <div style={{ position: 'absolute', left: 80, top: 170, display: 'flex', gap: 36, right: 80 }}>
      <Pane accent={C.cli} head="fleetc — the control surface" note="Mechanical CLI. Dumb, deterministic verbs over a Unix socket to the daemon. Scripts stay dumb."
        style={{ flex: 1 }}>
        <VerbList color={C.cli} items={[
          ['dispatch', 'queue an order for a ticket (--ac, --worktree, --hand)'],
          ['convoy open / close', 'group tickets into a pass of work for a captain'],
          ['hold · anchor · weigh · sink', 'steer the fleet — park, pin, release, cancel'],
          ['status', 'a cog reports its own state back to the daemon'],
          ['launch · review · reply', 'open the MR, read threads, answer them'],
          ['rebase · ci', 'forge-neutral rebase and pipeline control'],
          ['export / import', "snapshot and restore a project's state"],
        ]} />
      </Pane>
      <Pane accent={C.skill} head="/fleet-* skills — the judgment layer" note="Prompted procedures that wrap operator & agent judgment, then actuate fleetc. Skills stay smart."
        style={{ flex: 1 }}>
        <VerbList color={C.skill} items={[
          ['/fleet-commission', 'draft a Ready ticket to the Definition of Ready'],
          ['/fleet-berth', 'cut a worktree + branch for a dispatch'],
          ['/fleet-launch', 'push and open the MR from the ticket'],
          ['/fleet-parley', 'validate & answer the open review threads'],
          ['/fleet-rechart', 'rebase the MR; resolve conflicts locally'],
          ['/fleet-repair', 'classify a broken pipeline and fix it'],
          ['/fleet-yield · -publish', "pawn a hand's work to a sailor; ship the viewer"],
        ]} />
      </Pane>
    </div>
    <div style={{ position: 'absolute', left: 80, bottom: 56, fontSize: 19, color: C.dim }}>
      The operator uses either — raw <code style={{ color: C.cli, font: `600 18px ${MONO}` }}>fleetc</code> verbs, or the skills that wrap them.
    </div>
  </div>
);

// ─── 5 · the engine (scheduler state machine) ────────────────────────────────
function SchedulerSM() {
  const node = (x: number, y: number, w: number, h: number, label: string, sub: string, color: string) => (
    <g key={label}>
      <rect x={x - w / 2} y={y - h / 2} width={w} height={h} rx={13} fill={C.panel2} stroke={color} strokeWidth={2.2} />
      <text x={x} y={y - 4} textAnchor="middle" fill={color} fontFamily={MONO} fontSize={23} fontWeight={700}>{label}</text>
      <text x={x} y={y + 19} textAnchor="middle" fill={C.dim} fontFamily={MONO} fontSize={13}>{sub}</text>
    </g>
  );
  const edge = (x1: number, y1: number, x2: number, y2: number, bidir: boolean) => (
    <line x1={x1} y1={y1} x2={x2} y2={y2} stroke="#4d5864" strokeWidth={2}
      markerEnd="url(#ah)" markerStart={bidir ? 'url(#ah2)' : undefined} />
  );
  const lbl = (x: number, y: number, t: string, c = C.dim) => (
    <text x={x} y={y} textAnchor="middle" fill={c} fontFamily={MONO} fontSize={14}>{t}</text>
  );
  return (
    <svg viewBox="0 0 1000 600" style={{ width: '100%', height: '100%' }}>
      <defs>
        <marker id="ah" markerWidth="11" markerHeight="11" refX="8" refY="4" orient="auto">
          <path d="M0,0 L9,4 L0,8 z" fill="#4d5864" />
        </marker>
        <marker id="ah2" markerWidth="11" markerHeight="11" refX="1" refY="4" orient="auto">
          <path d="M9,0 L0,4 L9,8 z" fill="#4d5864" />
        </marker>
      </defs>
      {/* edges first, nodes drawn over them */}
      {edge(500, 262, 500, 108, true)}
      {edge(586, 300, 754, 300, true)}
      {edge(500, 338, 500, 494, true)}
      {edge(236, 300, 414, 300, false)}
      {edge(440, 336, 222, 494, false)}
      {edge(860, 332, 860, 494, false)}
      {edge(600, 528, 748, 528, true)}
      {lbl(560, 195, 'every ~5s')}
      {lbl(670, 285, 'every ~5 min')}
      {lbl(612, 420, 'command · or every ~60s')}
      {lbl(325, 285, 'command')}
      {lbl(240, 420, 'fleetd stopped', C.external)}
      {lbl(900, 420, 'writes')}
      {lbl(674, 512, 'reads · writes')}
      {node(500, 300, 170, 78, 'loop', 'wait ≤ 0.5s', C.text)}
      {node(500, 72, 240, 64, 'observe', 'who is alive · read-only', C.cli)}
      {node(860, 300, 210, 64, 'sync', 'pulls Jira + GitLab', C.adapter)}
      {node(500, 528, 200, 64, 'actuate', 'apply · reconcile', C.cog)}
      {node(150, 300, 170, 64, 'fleetc', 'human · agent', C.human)}
      {node(150, 522, 150, 56, 'exit', 'factory off', C.external)}
      {node(860, 528, 220, 64, 'ledger', 'SQLite · the database', C.sqlite)}
    </svg>
  );
}
const Engine: Page = () => (
  <div style={stage}>
    <Title kicker="the daemon">The Engine</Title>
    <div style={{ position: 'absolute', left: 60, top: 150, width: 1120, height: 760 }}>
      <SchedulerSM />
    </div>
    <div style={{ position: 'absolute', right: 70, top: 200, width: 600 }}>
      <Pane accent={C.daemon} head="scheduler.rs :: run">
        <div style={{ fontSize: 19, color: C.text, lineHeight: 1.5, marginTop: 10 }}>
          One thread, one loop. Each pass runs whatever is <b style={{ color: C.text }}>due</b>,
          then waits up to 0.5 s for a command.
        </div>
        <ul style={{ fontSize: 18, color: C.dim, lineHeight: 1.5, marginTop: 14, paddingLeft: 22 }}>
          <li><b style={{ color: C.cli }}>observe</b> checks which agent sessions are alive. Writes nothing.</li>
          <li><b style={{ color: C.adapter }}>sync</b> copies Jira tickets and GitLab MRs into the ledger, the database.</li>
          <li><b style={{ color: C.cog }}>actuate</b> changes the world: at once for a <b style={{ color: C.human }}>fleetc</b> command, and every ~60s to close the gap between intent and reality.</li>
          <li><b style={{ color: C.external }}>exit</b>: stop fleetd and the loop ends. Nothing new gets scheduled.</li>
        </ul>
      </Pane>
    </div>
  </div>
);

// ─── 6 · reconcile (the gap-closer) ──────────────────────────────────────────
const Reconcile: Page = () => (
  <div style={stage}>
    <Title kicker="reconcile.rs">The gap-closer</Title>
    <div style={{ position: 'absolute', left: 80, top: 162, right: 80, display: 'flex', gap: 24, fontSize: 20 }}>
      <div style={{ flex: 1, background: C.panel, border: `1px solid ${C.border}`, borderRadius: 14, padding: '18px 24px' }}>
        <div style={{ font: `700 15px ${MONO}`, color: C.accent }}>INTENT</div>
        <div style={{ color: C.dim, marginTop: 6 }}>orders on the queue · the board · convoys</div>
      </div>
      <div style={{ alignSelf: 'center', font: `700 26px ${MONO}`, color: C.dim }}>vs</div>
      <div style={{ flex: 1, background: C.panel, border: `1px solid ${C.border}`, borderRadius: 14, padding: '18px 24px' }}>
        <div style={{ font: `700 15px ${MONO}`, color: C.cog }}>REALITY</div>
        <div style={{ color: C.dim, marginTop: 6 }}>live sessions · ships on the forge · the tracker</div>
      </div>
    </div>
    <div style={{ position: 'absolute', left: 80, top: 300, right: 80 }}>
      <div style={{ display: 'grid', gridTemplateColumns: '1fr 1fr', gap: '14px 40px' }}>
        <Row color={C.cog} glyph="▸" name="spawn" role="a Ready, dispatched ticket with no session → launch the crew in its worktree" />
        <Row color={C.cog} glyph="▸" name="handoff" role="a builder reported concluded → tracker → In QA, request review, reap the builder" />
        <Row color={C.cog} glyph="▸" name="reap" role="a merged ship / terminal ticket → tracker → Done, end sessions, reclaim the worktree" />
        <Row color={C.cog} glyph="▸" name="heal" role="a dead worker/hand that didn't stop = a crash → respawn in place, resume its drop" />
        <Row color={C.dim} glyph="▸" name="+ captain / lookout" role="launch & stand-down the convoy captain and the allied-MR lookout" />
        <Row color={C.dim} glyph="▸" name="+ block" role="mirror a ticket's blocked-by edge onto the forge's MR dependency" />
      </div>
    </div>
    <div style={{ position: 'absolute', left: 80, bottom: 54, right: 80, fontSize: 19, color: C.dim, lineHeight: 1.4 }}>
      <b style={{ color: C.text }}>plan()</b> reads only; <b style={{ color: C.text }}>tick()</b> applies — a per-action failure is never fatal, the next tick retries.
      {' '}<code style={{ color: C.warn, font: `600 18px ${MONO}` }}>dry_run</code> plans and logs, executes nothing.
    </div>
  </div>
);

// ─── 7 · the crew ─────────────────────────────────────────────────────────────
const Crew: Page = () => (
  <div style={stage}>
    <Title kicker="who does the work">The crew</Title>
    <div style={{ position: 'absolute', left: 80, top: 170, right: 80, display: 'flex', gap: 36 }}>
      <Pane accent={C.cog} head="THE LINE — cogs bound by reconcile" note="One Claude session in a tmux pane. Each is one-shot or crash-healed; reconcile owns their lifecycle."
        style={{ flex: 1.15 }}>
        <div style={{ marginTop: 10 }}>
          <Row color={C.cog} glyph="⚙" name="captain" role="one per convoy — dispatches its clean frontier, then exits" />
          <Row color={C.cog} glyph="⚙" name="worker" role="one ticket in one worktree — packet → open MR, reports concluded" />
          <Row color={C.cog} glyph="⚙" name="hand" role="operator-driven worker — you write the code, it assists" />
          <Row color={C.cog} glyph="⚙" name="sailor" role="one open MR → merged — one sweep/pass: rebase · parley · repair · arm" />
          <Row color={C.cog} glyph="⚙" name="lookout" role="singleton — reviews one allied MR per pass" />
        </div>
      </Pane>
      <Pane accent={C.expert} head="STANDING EXPERTS — judgment, not the merge chain"
        style={{ flex: 1 }}>
        <div style={{ marginTop: 10 }}>
          <Row color={C.expert} glyph="🧭" name="admiral" role="design authority + fleet commander — grooms the backlog to Ready, opens convoys, wields fleetc" />
          <Row color={C.expert} glyph="🧭" name="shipwright" role="the armada's own machinery — develops the kit with the operator" />
          <Row color={C.expert} glyph="🧭" name="surveyor" role="independent reviewer — grades a diff against the ticket & the Definition of Done" />
        </div>
      </Pane>
    </div>
    <div style={{ position: 'absolute', left: 80, bottom: 54, fontSize: 19, color: C.dim }}>
      A cog is one file — <code style={{ color: C.text, font: `600 18px ${MONO}` }}>.claude/agents/&lt;name&gt;.md</code>: description · context · skills · initial prompt.
    </div>
  </div>
);

// ─── 8 · a ticket's journey (the line) ───────────────────────────────────────
function Stage({ n, status, title, actor, ship }: { n: number; status: string; title: string; actor: string; ship?: string }) {
  return (
    <div style={{ flex: 1, background: C.panel, border: `1px solid ${C.border}`, borderRadius: 12, padding: '16px 14px', position: 'relative' }}>
      <div style={{ font: `700 13px ${MONO}`, color: C.accent, letterSpacing: '0.1em' }}>{status}</div>
      <div style={{ fontSize: 19, fontWeight: 600, marginTop: 8, lineHeight: 1.2 }}>{title}</div>
      <div style={{ fontSize: 15, color: C.dim, marginTop: 8, lineHeight: 1.35 }}>{actor}</div>
      {ship && <div style={{ font: `600 12px ${MONO}`, color: C.external, marginTop: 10 }}>ship: {ship}</div>}
    </div>
  );
}
const TicketJourney: Page = () => (
  <div style={stage}>
    <Title kicker="the line">A ticket's journey</Title>
    <div style={{ position: 'absolute', left: 70, top: 220, right: 70, display: 'flex', gap: 12, alignItems: 'stretch' }}>
      <Stage n={1} status="TO DO" title="Groomed" actor="admiral grooms to Ready; blockers wired as blocked-by edges" />
      <Stage n={2} status="QUEUED" title="Dispatched" actor="an order lands on the queue (fleetc / a captain)" />
      <Stage n={3} status="IN PROGRESS" title="Implementing" actor="reconcile spawns a worker in the worktree" ship="Draft → Open" />
      <Stage n={4} status="IN QA" title="Handoff" actor="worker concluded → In QA, review requested, builder reaped" ship="Open" />
      <Stage n={5} status="IN QA" title="Finishing" actor="a sailor sweeps: rebase · parley · repair · arm merge" ship="Open" />
      <Stage n={6} status="DONE" title="Reaped" actor="ship merges → Done, sessions ended, worktree reclaimed" ship="Merged" />
    </div>
    <div style={{ position: 'absolute', left: 70, top: 196, right: 70, height: 2, background: `linear-gradient(90deg, ${C.accent}, ${C.cog})` }} />
    <div style={{ position: 'absolute', left: 80, bottom: 70, right: 80, fontSize: 21, color: C.text, lineHeight: 1.4 }}>
      <b style={{ color: C.cog }}>Invariant:</b> reconcile owns <i>every</i> tracker transition — cogs report their own state, they never move the ticket themselves.
    </div>
  </div>
);

// ─── 9 · the ledger (live data-model graph) ──────────────────────────────────
const LedgerLive: Page = () => (
  <div style={stage}>
    <Title kicker="the single source of truth">The ledger</Title>
    <div style={{ position: 'absolute', left: 60, top: 150, width: 1800, height: 900 }}>
      <FleetGraph graph={dataGraph} />
    </div>
  </div>
);

// ─── 10 · quality gates (DoR / DoD) ──────────────────────────────────────────
const QualityGates: Page = () => (
  <div style={stage}>
    <Title kicker="/fleet-ready-done">Quality gates</Title>
    <div style={{ position: 'absolute', left: 80, top: 170, right: 80, display: 'flex', gap: 36 }}>
      <Pane accent={C.accent} head="Definition of Ready — the dispatch gate" note="A captain won't dispatch a ticket that isn't Ready."
        style={{ flex: 1 }}>
        <ul style={{ fontSize: 20, color: C.text, lineHeight: 1.6, marginTop: 14, paddingLeft: 22 }}>
          <li>status is <b>To&nbsp;Do</b> and the ticket is <b>not held</b></li>
          <li>every blocker is <b>terminal</b> (Done / Cancelled)</li>
          <li>structured to the four-section body — context, scope, acceptance criteria, out-of-scope</li>
          <li>dependencies wired as formal <code style={{ color: C.accent, font: `600 18px ${MONO}` }}>blocked-by</code> edges</li>
        </ul>
      </Pane>
      <Pane accent={C.cog} head="Definition of Done — the merge gate" note="The worker and the reviewer judge the MR against it before concluding."
        style={{ flex: 1 }}>
        <ul style={{ fontSize: 20, color: C.text, lineHeight: 1.6, marginTop: 14, paddingLeft: 22 }}>
          <li>satisfies the ticket's <b>acceptance criteria</b></li>
          <li><b>reviewed</b> — surveyor clean, every thread resolved</li>
          <li><b>CI green</b> and the MR is <b>out of draft</b></li>
          <li>the forge gates the actual merge: pipeline + threads + approvals</li>
        </ul>
      </Pane>
    </div>
    <div style={{ position: 'absolute', left: 80, bottom: 56, fontSize: 20, color: C.dim }}>
      The kit's floor — a project's own stricter doctrine (in its repo) extends it.
    </div>
  </div>
);

// ─── demo (video placeholder) ───────────────────────────────────────────────
const Demo: Page = () => (
  <div style={stage}>
    <Title kicker="see it run">Demo</Title>
    {/* Video goes here — swap the placeholder for a <video>/<iframe> when the link is ready. */}
    <div style={{
      position: 'absolute', left: 160, top: 210, width: 1600, height: 760,
      border: `2px dashed ${C.border}`, borderRadius: 14,
      display: 'flex', alignItems: 'center', justifyContent: 'center', fontSize: 30, color: C.dim,
    }}>
      Video — coming soon
    </div>
  </div>
);

// ─── q&a ─────────────────────────────────────────────────────────────────────
const QA: Page = () => (
  <div style={stage}>
    <div style={{ position: 'absolute', left: 120, top: 440, fontSize: 132, fontWeight: 700, fontFamily: 'var(--osd-font-display)', letterSpacing: '-0.03em' }}>Q&amp;A</div>
  </div>
);

// ─── 11 · close (evolution, bottom-up) ───────────────────────────────────────
function Step({ n, title, body, color }: { n: string; title: string; body: string; color: string }) {
  return (
    <div style={{ flex: 1, background: C.panel, border: `1px solid ${C.border}`, borderLeft: `4px solid ${color}`, borderRadius: 12, padding: '22px 24px' }}>
      <div style={{ font: `700 34px ${MONO}`, color }}>{n}</div>
      <div style={{ fontSize: 26, fontWeight: 600, marginTop: 10 }}>{title}</div>
      <div style={{ fontSize: 18, color: C.dim, marginTop: 10, lineHeight: 1.45 }}>{body}</div>
    </div>
  );
}
const Close: Page = () => (
  <div style={stage}>
    <Title kicker="how it grew">Bottom-up, not top-down</Title>
    <div style={{ position: 'absolute', left: 80, top: 220, right: 80, display: 'flex', gap: 28 }}>
      <Step n="①" color={C.skill} title="A skill" body="One SKILL.md — a prompted, repeatable procedure, written the first time a job was worth encoding once. Captured judgment." />
      <Step n="②" color={C.cli} title="Skills + mechanics" body="A workflow needs several skills plus deterministic glue — fleetc and the daemon actuate the mechanical parts. Scripts stay dumb, skills stay smart." />
      <Step n="③" color={C.cog} title="A cog" body="Once a role's worth of skills + context recurs, encapsulate it into one .claude/agents/<name>.md. Every cog is this same bottom-up move." />
    </div>
    <div style={{ position: 'absolute', left: 80, bottom: 90, right: 80 }}>
      <div style={{ fontSize: 40, fontWeight: 600, lineHeight: 1.3 }}>
        The factory grew from the smallest useful unit outward —
        <span style={{ color: C.accent }}> and it is always on.</span>
      </div>
    </div>
  </div>
);

const pages: Page[] = [
  Cover,
  GettingStarted,
  Sdlc,
  PhaseTickets,
  PhaseDispatch,
  PhaseImplementation,
  PhaseMerge,
  PhaseDone,
  Sightline,
  Engine,
  Demo,
  QA,
];

// Parked — kept in source for ideas / a later revamp, but not shown in the deck.
// To bring one back, add it to `pages` and move its note from `parkedNotes` into `notes`.
void [TwoInterfaces, LedgerLive, Reconcile, Crew, TicketJourney, QualityGates, Close];

export const notes: (string | undefined)[] = [
  `The Armada — Factory's always-on agentic software factory. Daniel Duclos-Cavalcanti, SWE II. The pitch in one breath: a daemon that runs Claude agents as a crew, and keeps the whole thing honest against a ledger.`,
  `Getting started — the 0-to-100 of an intelligent loop, built one beat at a time. ① A process worth improving: a failing pipeline that a human babysits by clicking through jobs. ② A skill: plain markdown that says "given an MR, find its pipeline, report every stage and job with its state". ③ We notice status-querying is deterministic, so a script moves into the skill folder — fewer tokens, same answer; the model spends tokens only on judgment. ④ An agent: one more markdown file — what it knows, may do, must do — whose initialPrompt is a /loop over the skill, diagnosing failed jobs against a runbook plus the repo and job config. ⑤ Like any algorithm it now has a loop, a goal, accepted states, and a break condition where it asks a human. That's a live, somewhat autonomous loop over a real slice of the SDLC — and every cog in the Armada is built this same way.`,
  `SDLC — the line at a glance. The operator files work as tickets (the admiral grooms them to Ready); a captain dispatches them; a worker (or a hand, with you at the keyboard) implements; a sailor lands the MR; reconcile reaps it to Done. When implementation or merge hits something it can't settle, it escalates to TRIAGE — the captain that dispatched the work (planned: today this isn't wired yet). The captain runs the same loop as any other process: if it's fixable, it resolves it and the cog resumes; if not, it goes back to the operator for a human decision.`,
  `Tickets. This is me: I bring intent — epics, stories — and an agent (the admiral, via /fleet-commission) drafts each one to the Definition of Ready: four-section body, testable acceptance criteria, dependencies wired as blocked-by edges. They land in Jira as To Do. I review, refine, prioritise — and the loop goes round until the backlog is Ready.`,
  `Dispatch. The orchestrator (the captain, in the factory's terms) consults the queue of related tickets and writes back to it. fleetd has already done the deterministic part — dependencies, blockers — so the orchestrator only makes the judgment call: is this ticket Ready? Yes → a worktree is cut and the ticket is dispatched. No → hold; once fixed it's re-queued.`,
  `Implementation. Reconcile sees the order and spawns a worker in its worktree (or a hand, when I want to write the code myself). It implements to the acceptance criteria with tests and lint, opens a draft MR, then has a reviewer agent review it in a fresh context. Fixes needed → fix and review again. Clean → concluded: fleetd hands off, ticket goes to In QA. If it crosses its threshold (today: five rounds) → escalate to triage.`,
  `Merge. The merge driver (the sailor) sweeps the open MR. It reads the MR's state and acts: conflicts → rebase, review threads → reply and apply the fixes found, failed CI → repair, clean → arm auto-merge. Then it sleeps — roughly a minute — and sweeps again. If something crosses its threshold, it escalates to triage, same as the worker. Once the forge's gates pass — pipeline, threads, approvals — the forge merges it.`,
  `Done. The merged MR has met the Definition of Done. On its next tick reconcile reaps it: Jira → Done, sessions end, the worktree is freed, and the ledger records one event. Closing the ticket clears blockers for its dependents — so the captain's next pass has a new frontier, and the line keeps moving.`,
  `The sightline — the whole system in one opaque view, rendered live by the same React-Flow renderer as the demos/fleet viewer (vendored, read-only), so it always matches the code. Operator → wraps judgment in /fleet-* skills → fleetc (the CLI) → fleetd (the daemon) → which actuates the tracker and forge, spawns the crew, works the project worktrees, and persists everything to the ledger, which the dashboard reads. Pan/zoom to walk it.`,
  `The engine — fleetd's heart is one loop on one thread (scheduler.rs::run). It isn't a pipeline: each pass just runs whatever is due, then waits up to half a second for a command. Observe, every ~5s: which agent sessions are alive — read-only, nothing written. Sync, every ~5 minutes: copy Jira tickets and GitLab MRs into the ledger, our database. Actuate: this is where the world changes — immediately when a fleetc command arrives (from me or from an agent), and every ~60s when reconcile compares intent with reality and spawns, hands off, reaps or heals. All writes go through this one thread, so there's never a race. Stop fleetd and the loop ends — the factory is off.`,
  `Demo — video placeholder; link to be added.`,
  `Open the floor for questions.`,
];

const parkedNotes: Record<string, string> = {
  TwoInterfaces: `Two interfaces, one surface. LEFT — fleetc: the mechanical control surface. Dumb, deterministic verbs spoken over a Unix socket to the daemon: dispatch, convoy, hold/anchor/weigh/sink, status, launch, rebase, ci, export/import. RIGHT — the /fleet-* skills: the judgment layer. Prompted procedures that carry operator and agent judgment and then actuate fleetc. The thesis of the whole kit: scripts stay dumb, skills stay smart. The operator can drive with either.`,
  LedgerLive: `The ledger — the single source of truth, rendered live from the data-model graph. A SQLite database in WAL mode with a single writer (the scheduler). Twelve tables around the domain entities — tickets, orders, sessions, ships and their snapshots, convoys, reports, events. It's event-sourced: every write appends a CDC event, so the dashboard and fleetc read a consistent picture without ever contending with the writer. Click any node in the viewer for its exact Rust struct or DDL.`,
  Reconcile: `reconcile — the gap-closer. This is where the factory actually runs itself. It compares INTENT (orders on the queue, the board, convoys) against REALITY (live sessions, ships on the forge, the tracker) and plans a set of moves: SPAWN a Ready dispatched ticket's crew, HANDOFF a concluded builder to a sailor (and move the ticket to In QA), REAP a merged ship (tracker → Done, reclaim the worktree), HEAL a crashed worker in place. Plus captain/lookout launches and mirroring blocked-by onto MR deps. plan() only reads; tick() applies; any single action failing is never fatal — the next tick just retries. dry_run plans and logs but executes nothing.`,
  Crew: `The crew. Every cog is one Claude session in a tmux pane. THE LINE is a hand-off chain reconcile binds: a captain dispatches a convoy's frontier; a worker implements one ticket in one worktree to an open MR; a hand is the same but operator-driven — you write the code; a sailor lands one open MR to merged with one sweep per pass (rebase, parley, repair, arm merge); a lookout is a singleton reviewing allied MRs. STANDING EXPERTS sit outside the merge chain for judgment: admiral (design + command), shipwright (the kit itself), surveyor (independent review). A cog is just one markdown file: description, context, skills, initial prompt.`,
  TicketJourney: `A ticket's journey — the line, left to right. Groomed (To Do, Ready) → Dispatched (an order on the queue) → Implementing (reconcile spawns a worker; the ship goes Draft→Open) → Handoff (worker concluded; ticket → In QA, review requested, builder reaped) → Finishing (a sailor sweeps: rebase/parley/repair, arms auto-merge) → Reaped (ship merges → ticket Done, sessions ended, worktree reclaimed). THE invariant: reconcile owns every tracker transition. Cogs report their own state; they never move the ticket themselves — that's what keeps intent and reality from forking.`,
  QualityGates: `Quality gates — the Definition of Ready and Done (/fleet-ready-done). READY is the dispatch gate, and it's enforced in code: a ticket is Ready only if it's To Do, not held, and every blocker is terminal — plus groomed to the four-section body with dependencies wired as blocked-by edges. DONE is the merge gate: the MR meets the ticket's acceptance criteria, is reviewed (surveyor clean, threads resolved), CI is green and it's out of draft — and the forge itself gates the real merge on pipeline, threads and approvals. This is the kit's floor; a project layers its own stricter doctrine on top.`,
  Close: `How it grew — bottom-up, never top-down. ① A skill: one SKILL.md, captured judgment, written the first time a job was worth encoding once. ② Skills plus mechanics: deterministic glue — fleetc and the daemon — actuate the mechanical parts; scripts stay dumb, skills stay smart. ③ A cog: once a role's skills and context recur, encapsulate them into one agent file. The factory was built the same way it builds — from the smallest useful unit outward — and it is always on.`,
};
void parkedNotes;

export const meta: SlideMeta = { title: 'The Armada — Agentic Software Factory' };
export default pages satisfies Page[];
