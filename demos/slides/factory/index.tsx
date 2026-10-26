import { type DesignSystem, type Page, type SlideMeta } from '@open-slide/core';
import type { CSSProperties } from 'react';
import evolution from './assets/evolution.svg';
import agentAnatomy from './assets/agent-anatomy.svg';
import lifecycle from './assets/lifecycle.svg';
import overview from './assets/overview.svg';
import converge from './assets/converge.svg';
import roster from './assets/roster.svg';
import foreman from './assets/foreman.svg';
import worker from './assets/worker.svg';
import finisher from './assets/finisher.svg';
import FleetGraph from './fleet/FleetGraph';
import overviewGraph from './fleet/overview.json';

export const design: DesignSystem = {
  palette: { bg: '#0b0b0d', text: '#e8eaed', accent: '#4aa3ff' },
  fonts: {
    display: '-apple-system, BlinkMacSystemFont, "Inter", system-ui, sans-serif',
    body: '-apple-system, BlinkMacSystemFont, "Inter", system-ui, sans-serif',
  },
  typeScale: { hero: 128, body: 30 },
  radius: 12,
};

const stage: CSSProperties = {
  width: '100%',
  height: '100%',
  position: 'relative',
  background: '#0b0b0d',
  overflow: 'hidden',
  color: '#e8eaed',
  fontFamily: 'var(--osd-font-body)',
};

// A full-bleed diagram page: title top-left, SVG stretched beneath it.
function diagram(title: string, svg: string): Page {
  const P: Page = () => (
    <div style={stage}>
      <div style={{ position: 'absolute', left: 80, top: 46, fontSize: 48, fontWeight: 600 }}>{title}</div>
      <img src={svg} alt={title} style={{ position: 'absolute', left: 80, top: 140, width: 1760, height: 880, objectFit: 'contain' }} />
    </div>
  );
  return P;
}

const Cover: Page = () => (
  <div style={stage}>
    <div style={{ position: 'absolute', left: 120, top: 400 }}>
      <div style={{ fontSize: 128, fontWeight: 700, fontFamily: 'var(--osd-font-display)', letterSpacing: '-0.02em' }}>Managed Custody</div>
      <div style={{ fontSize: 56, fontWeight: 600, color: '#4aa3ff', marginTop: 8 }}>{'Agentic Software Factory - '}<span style={{ fontStyle: 'italic' }}>Armada</span></div>
      <div style={{ fontSize: 28, color: '#9aa0a6', marginTop: 28 }}>Daniel Duclos-Cavalcanti — SWE II</div>
    </div>
  </div>
);

// PoC: the live Overview graph embedded as a React component (no SVG, no iframe)
// — the same renderer as demos/fleet, so it always matches the code.
const LiveOverview: Page = () => (
  <div style={stage}>
    <div style={{ position: 'absolute', left: 80, top: 46, fontSize: 48, fontWeight: 600 }}>The sightline — live</div>
    <div style={{ position: 'absolute', left: 80, top: 140, width: 1760, height: 880 }}>
      <FleetGraph graph={overviewGraph} />
    </div>
  </div>
);

const pages: Page[] = [
  Cover,
  LiveOverview,
  diagram('Factory 0 – 100', evolution),
  diagram('Anatomy of an Agent', agentAnatomy),
  diagram('Factory SDLC', lifecycle),
  diagram('Factory State-Machine', overview),
  diagram('converge.sh — per-tick logic', converge),
  diagram('Factory Agents', roster),
  diagram('Foreman · Dispatch, Orchestration', foreman),
  diagram('Worker · Implement', worker),
  diagram('Finisher · Merge', finisher),
];

export const notes: (string | undefined)[] = [
  `Managed Custody — the software factory. Daniel Duclos-Cavalcanti, SWE II.`,
  `PoC — the live Overview, rendered by the same React-Flow renderer as the demos/fleet viewer (vendored read-only). The whole system in one sightline: operator → fleetc → fleetd → crew → project. No static SVG, no iframe — it always matches the code.`,
  `Bottom-up, not top-down. The factory grew from the smallest useful unit outward.
① A skill — one SKILL.md, a prompted repeatable procedure, written the first time a job was worth encoding once (/tdd, /mr-review). Captured judgment.
② Skills + scripts — a workflow needs several skills plus deterministic glue; scripts (converge.sh, queue.sh, classify.py) actuate the mechanical parts. The thesis: scripts stay dumb, skills stay smart.
③ An agent — once a role's worth of skills + context recurs, encapsulate it into one .claude/agents/<name>.md. That bundle is a cog. Same bottom-up move made every cog.`,
  `What stage ③ contains: an agent is one Markdown file (frontmatter + system-prompt body) encapsulating four things.
📝 Description — the one-liner: what the cog is and exactly when to spawn it (precise triggers, no charter overlap).
🧠 Context — the grounding it reasons from: preloaded canon, schemas, and MANIFEST.md as source of truth.
🧰 Skills — an enumeration of what it may call (scoped to the role); also the guardrail.
▶ Initial prompt — the standing system prompt: lifecycle + invariants. The cog is born knowing its job.
Every cog that follows is this same four-slot shape.`,
  `The line (top band): one ticket walks left to right.
GROOMED (Jira To Do, past DoR) → DISPATCHED (foreman wrote packet + worktree; Jira untouched) → IMPLEMENTING (converge claimed In Progress, spawned a worker on /mady-implement) → CONCLUDED (reviewed MR open; Jira In QA, finisher spawned) → FINISHING (rebase, threads, CI, arm merge) → MERGED → DONE (converge transitions Jira Done, retires the packet).
Exceptions & holds (bottom band): any cog can escalate or be parked; state files carry it across a crash. Key invariant: converge owns every Jira transition — cogs never move the ticket themselves.`,
  `converge.sh is the actuation core — pure deterministic mechanics, no LLM. Cron fires it every 5 minutes. Each tick reconciles three surfaces — disk + tmux + Jira — idempotent at restart.
Five steps per tick: spawn_workers (pending packet → claim Jira, launch worker) · spawn_finishers (concluded worker → launch finisher, In Progress → In QA) · reconcile_cogs (crashed non-terminal cog → respawn) · reconcile_foremen (one foreman per queue board) · conclude_dispatches (merged finisher → Jira Done).
It is the sole Jira-transition point for dispatched work. Foreman writes intent, converge actuates, agents write filesystem state, dashboard reads it. Scripts stay dumb, skills stay smart.`,
  `The tick, unrolled. main() runs five reconcile steps in fixed order, then writes its snapshot. Each step loops over files on disk with a guard (◈); it fires the action or records an [ANOMALY] and moves on — one ticket's failure is never fatal, the next tick retries.
write_snapshot composes the tick's record (atomic, schema-validated) into converge.json / .jsonl — the channel the idle foreman reads.
Why safe: every gate is single-shot per ticket, so a re-run re-derives from disk + tmux + Jira and never double-acts. Pure mechanics, no LLM — which is why it's a cron script, not an agent loop.`,
  `How agents are used. The factory is run by a crew of cogs — each a Claude session in its own tmux pane. Three tiers:
The line (R1 → R2 → R3) — a hand-off chain bound by converge.sh: foreman selects & dispatches, worker implements, finisher merges.
Parallel — the auditor, a standing singleton that reviews peer MRs by SHA (never the factory's own output).
Expert tier — standing domain authorities consulted for judgment, not part of the merge chain: draftsman (design canon & grooming), millwright (the kit's own machinery), prover (live-product conformance).`,
  `Creating the work. The foreman is the operator's single console — idle by default, chat-driven, writes no status drop. It works only from its queue (~/.mady/state/queue/) — not a backlog scan — and its durable output is dispatch packets. It also watches the cogs it dispatched.
Per queued ticket: assemble (candidate.sh — sub-task gate, resolve blockers) → classify (classify.py — READY-CLEAN / READY-STACKED / BLOCKED-WAIT over the dependency DAG) → order → dispatch (materialise worktree, write the schema-valid packet via dispatch.sh). Upstream, the draftsman groomed the ticket to the Definition of Ready.
Two judgment levers in the packet: review_gate (human when it touches an ADD) and park_at (stop before the MR).`,
  `Implementing the work. A worker owns one ticket, packet to open MR. Spawned inside the ticket's worktree, it derives its ticket key from the git branch, reads the packet, and invokes /mady-implement — which routes to a shape-specific phase (logic / infra / docs / claude) and drives RED → GREEN → REFACTOR per acceptance criterion.
It concludes when the MR is open, self-reviewed, and out of draft — the terminal state concluded that triggers converge to hand off to a finisher. Non-terminal exits: escalated (blocked, refute-first) or parked (held).`,
  `Completing the work. A finisher owns one open MR to merged. Each tick it runs /mady-finish — a punch list: rebase if divergent (unwind first if stacked and the parent merged), reply to review threads, classify-and-fix broken CI, and aggressively arm merge_when_pipeline_succeeds.
GitLab gates the actual merge on pipeline + threads + approvals, and every push disarms an armed merge — which is why the finisher re-arms aggressively. A draft MR is a hard "do not merge." Terminal: merged / closed — it records the state and exits; converge owns the Jira Done transition.`,
];

export const meta: SlideMeta = { title: 'Managed Custody — Software Factory' };
export default pages satisfies Page[];
