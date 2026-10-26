import { type DesignSystem, type Page, type SlideMeta } from '@open-slide/core';
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
      <div style={{ font: `700 22px ${MONO}`, letterSpacing: '0.2em', color: C.accent, textTransform: 'uppercase' }}>Managed Custody</div>
      <div style={{ fontSize: 132, fontWeight: 700, fontFamily: 'var(--osd-font-display)', letterSpacing: '-0.03em', marginTop: 10 }}>The Armada</div>
      <div style={{ fontSize: 40, fontWeight: 500, color: C.dim, marginTop: 8 }}>An always-on agentic software factory</div>
      <div style={{ fontSize: 26, color: C.dim, marginTop: 40 }}>Daniel Duclos-Cavalcanti — SWE II</div>
    </div>
  </div>
);

// ─── 2 · the sightline (live overview graph) ─────────────────────────────────
const Sightline: Page = () => (
  <div style={stage}>
    <Title kicker="the whole system">The sightline</Title>
    <div style={{ position: 'absolute', left: 60, top: 150, width: 1800, height: 900 }}>
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
      {edge(576, 300, 756, 300, true)}
      {edge(500, 338, 500, 494, true)}
      {edge(424, 300, 230, 300, true)}
      {edge(440, 336, 222, 494, false)}
      {lbl(500, 195, 'deadline ~5s')}
      {lbl(668, 285, 'deadline ~300s')}
      {lbl(500, 420, 'deadline ~60s')}
      {lbl(325, 285, 'Ok(cmd)')}
      {lbl(290, 430, 'Disconnected', C.external)}
      {node(500, 300, 158, 78, 'wait', 'recv ≤ 500ms', C.text)}
      {node(500, 72, 150, 64, 'poll', '~5s · read-only', C.cli)}
      {node(830, 300, 158, 64, 'sync', '~300s · write', C.adapter)}
      {node(500, 528, 160, 64, 'reconcile', '~60s · write', C.cog)}
      {node(150, 300, 150, 64, 'apply', 'cmd · write', C.dim)}
      {node(150, 522, 150, 56, 'exit', 'channel closed', C.external)}
    </svg>
  );
}
const Engine: Page = () => (
  <div style={stage}>
    <Title kicker="the daemon">The engine — one single-writer loop</Title>
    <div style={{ position: 'absolute', left: 60, top: 150, width: 1120, height: 760 }}>
      <SchedulerSM />
    </div>
    <div style={{ position: 'absolute', right: 70, top: 200, width: 600 }}>
      <Pane accent={C.daemon} head="scheduler.rs :: run">
        <div style={{ fontSize: 19, color: C.text, lineHeight: 1.5, marginTop: 10 }}>
          Each pass fires any <b style={{ color: C.text }}>ticker</b> whose deadline has
          passed, then blocks on the command channel for ≤500 ms and loops.
        </div>
        <ul style={{ fontSize: 18, color: C.dim, lineHeight: 1.5, marginTop: 14, paddingLeft: 22 }}>
          <li><b style={{ color: C.cli }}>poll</b> refreshes the in-memory liveness snapshot — no ledger write, no event.</li>
          <li><b style={{ color: C.adapter }}>sync</b>, <b style={{ color: C.cog }}>reconcile</b> and each client <b style={{ color: C.dim }}>command</b> are the writes — one writer, one transaction, one CDC event.</li>
          <li>A <b style={{ color: C.external }}>Disconnected</b> channel breaks the loop.</li>
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
  Sightline,
  TwoInterfaces,
  Engine,
  Reconcile,
  Crew,
  TicketJourney,
  LedgerLive,
  QualityGates,
  Close,
];

export const notes: (string | undefined)[] = [
  `The Armada — Managed Custody's always-on agentic software factory. Daniel Duclos-Cavalcanti, SWE II. The pitch in one breath: a daemon that runs Claude agents as a crew, and keeps the whole thing honest against a ledger.`,
  `The sightline — the whole system in one opaque view, rendered live by the same React-Flow renderer as the demos/fleet viewer (vendored, read-only), so it always matches the code. Operator → wraps judgment in /fleet-* skills → fleetc (the CLI) → fleetd (the daemon) → which actuates the tracker and forge, spawns the crew, works the project worktrees, and persists everything to the ledger, which the dashboard reads. Pan/zoom to walk it.`,
  `Two interfaces, one surface. LEFT — fleetc: the mechanical control surface. Dumb, deterministic verbs spoken over a Unix socket to the daemon: dispatch, convoy, hold/anchor/weigh/sink, status, launch, rebase, ci, export/import. RIGHT — the /fleet-* skills: the judgment layer. Prompted procedures that carry operator and agent judgment and then actuate fleetc. The thesis of the whole kit: scripts stay dumb, skills stay smart. The operator can drive with either.`,
  `The engine. fleetd's heart is one single-writer scheduling loop (scheduler.rs::run). Each pass fires any TICKER whose deadline has passed, then blocks on the command channel for up to 500ms and loops. poll (~5s) is READ-ONLY — it only refreshes the in-memory liveness snapshot, no ledger write, no event. sync (~300s), reconcile (~60s), and each client command are the writes — one writer, one transaction, one change-data-capture event. If the socket's gone, the channel Disconnects and the loop exits. Cadences are config defaults, re-read live each pass.`,
  `reconcile — the gap-closer. This is where the factory actually runs itself. It compares INTENT (orders on the queue, the board, convoys) against REALITY (live sessions, ships on the forge, the tracker) and plans a set of moves: SPAWN a Ready dispatched ticket's crew, HANDOFF a concluded builder to a sailor (and move the ticket to In QA), REAP a merged ship (tracker → Done, reclaim the worktree), HEAL a crashed worker in place. Plus captain/lookout launches and mirroring blocked-by onto MR deps. plan() only reads; tick() applies; any single action failing is never fatal — the next tick just retries. dry_run plans and logs but executes nothing.`,
  `The crew. Every cog is one Claude session in a tmux pane. THE LINE is a hand-off chain reconcile binds: a captain dispatches a convoy's frontier; a worker implements one ticket in one worktree to an open MR; a hand is the same but operator-driven — you write the code; a sailor lands one open MR to merged with one sweep per pass (rebase, parley, repair, arm merge); a lookout is a singleton reviewing allied MRs. STANDING EXPERTS sit outside the merge chain for judgment: admiral (design + command), shipwright (the kit itself), surveyor (independent review). A cog is just one markdown file: description, context, skills, initial prompt.`,
  `A ticket's journey — the line, left to right. Groomed (To Do, Ready) → Dispatched (an order on the queue) → Implementing (reconcile spawns a worker; the ship goes Draft→Open) → Handoff (worker concluded; ticket → In QA, review requested, builder reaped) → Finishing (a sailor sweeps: rebase/parley/repair, arms auto-merge) → Reaped (ship merges → ticket Done, sessions ended, worktree reclaimed). THE invariant: reconcile owns every tracker transition. Cogs report their own state; they never move the ticket themselves — that's what keeps intent and reality from forking.`,
  `The ledger — the single source of truth, rendered live from the data-model graph. A SQLite database in WAL mode with a single writer (the scheduler). Twelve tables around the domain entities — tickets, orders, sessions, ships and their snapshots, convoys, reports, events. It's event-sourced: every write appends a CDC event, so the dashboard and fleetc read a consistent picture without ever contending with the writer. Click any node in the viewer for its exact Rust struct or DDL.`,
  `Quality gates — the Definition of Ready and Done (/fleet-ready-done). READY is the dispatch gate, and it's enforced in code: a ticket is Ready only if it's To Do, not held, and every blocker is terminal — plus groomed to the four-section body with dependencies wired as blocked-by edges. DONE is the merge gate: the MR meets the ticket's acceptance criteria, is reviewed (surveyor clean, threads resolved), CI is green and it's out of draft — and the forge itself gates the real merge on pipeline, threads and approvals. This is the kit's floor; a project layers its own stricter doctrine on top.`,
  `How it grew — bottom-up, never top-down. ① A skill: one SKILL.md, captured judgment, written the first time a job was worth encoding once. ② Skills plus mechanics: deterministic glue — fleetc and the daemon — actuate the mechanical parts; scripts stay dumb, skills stay smart. ③ A cog: once a role's skills and context recur, encapsulate them into one agent file. The factory was built the same way it builds — from the smallest useful unit outward — and it is always on.`,
];

export const meta: SlideMeta = { title: 'The Armada — Agentic Software Factory' };
export default pages satisfies Page[];
