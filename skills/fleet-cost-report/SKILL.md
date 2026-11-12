---
description: >-
  Report the fleet's spend as one self-contained HTML dashboard, from its telemetry: the total, the
  last 24 h and 7 days, the cost per Done ticket, the cache hit rate, and USD by day, role (with its
  subagents' share), convoy, ticket and model. Read-only. For the shipwright, on the operator's order.
argument-hint: "[projects/<p>] [--since <date|all>]"
allowed-tools: Bash(${CLAUDE_SKILL_DIR}/scripts/report.sh *)
---

# Skill: fleet-cost-report

One page on what the fleet has spent, from the telemetry fleetd collects off every Claude session
(each API request's cost, tokens, model, and the cog's role and ticket). Scripted end to end.

## Run

```bash
${CLAUDE_SKILL_DIR}/scripts/report.sh projects/<p> [--since <YYYY-MM-DD|RFC3339|all>] [<out.html>]
```

The window opens on the first of the current month (UTC). `--since` moves it to another date, and `--since all` covers the whole record.

It prints the page's path (default `reports/<p>/costs/fleet-spend_<UTC date>.html`; a second
run that day replaces it), then the headline figures as JSON. Open the page (`open <path>`).

- `scripts/cost.sql` is every figure, as one JSON object: the telemetry DB read-only, with the
  ledger attached read-only for each ticket's convoy, title and status.
- `template.html` renders it; the data rides in it as JSON.

**Attribution.** A request is its cog's role; one with no role is `interactive` (a session the
operator started, as `fleetc cost` has it). A subagent's request (it carries an `agent_id`) counts
to the role that spawned it; **By role** fades that share. A ticket's spend counts to the convoy it sails in, and
a captain's to its own; an ally's MR or a ticket outside every convoy counts as outside the
convoys. **Per Done ticket** is the spend recorded on tickets now Done, over how many they are.

## Hand off

The path, then one line: `$total (window) · $24h last 24 h · $7d last 7 days · $per Done ticket ·
top role`. Add one more line naming what stands out, such as a cancelled ticket among the costliest,
or a role or convoy spending out of proportion to what it delivered. That line is your judgement;
the page is measured.
