---
description: >-
  Propose work found mid-ticket that isn't yours — a bug nearby, debt worth its own ticket, a
  reviewer's out-of-scope ask, or an existing ticket that needs taking on (e.g. one blocking
  your merge, sitting unassigned). Drafts a new ticket to /fleet-ready-done, or points at the
  existing one, then proposes it with `fleetc propose`: a new one is filed at once, labelled
  `fleet-proposed` and outside any convoy, for the operator (or the admiral) to accept or drop
  (cancel). For a worker, sailor or hand (the admiral and the operator file: /fleet-commission).
argument-hint: "[type]"
---

# Skill: fleet-propose

You found work that isn't your ticket's. It doesn't widen your diff, and it doesn't vanish as a
note or sit inside an escalation: you propose it. **The judgment is yours:** what the work is
and why it matters. **The decision is the operator's:** `fleetc propose` files a new ticket
labelled `fleet-proposed`, outside any convoy so no captain dispatches it, and they (or the
admiral) accept it (the label comes off) or drop it (it's cancelled).

**Argument:** `type` (optional) — `story`, `bug`, `task`, or `epic`, for a new ticket. If
omitted, pick the one that fits per `/fleet-ready-done`, and say which.

## Step 1 — Is there a ticket already?

Search the board for the work's key terms (filesystem tracker: `grep -ril "<term>" <tickets
folder>`; Jira: its search).

- **One exists** → Step 2a: propose taking it on.
- **None does** → Step 2b: draft a new one.

## Step 2a — Propose an existing ticket

```bash
fleetc propose --existing <KEY> --why "<one line: what it blocks or why it matters now>"
```

The why is the case for the operator: what it blocks (your merge, every merge train), and its
state (To Do, unassigned) if that's why it's stalled. On accept it joins your ticket's convoy;
add `--convoy <KEY>` only if it belongs in another.

## Step 2b — Propose a new ticket

Follow `/fleet-ready-done`'s *Drafting a ticket* (draft, wire, check Ready — Step 1 above was
its duplicate check). Leave `--convoy` off unless it belongs elsewhere: the proposal inherits
your ticket's convoy.

```bash
fleetc propose --type <type> --title "<title>" --blocked-by <KEY> … --body - <<'EOF'
## Outcome
…
EOF
```

## Then

Either prints the proposal's id and the ticket's key. Cite the key where the work came up (a
reviewer's thread: "proposed as <KEY>", then resolve it). Say so in one line
(`proposed #<id> as <KEY>: <what>`), then get back to your ticket.

## Boundaries

You propose; you never file outside a proposal, assign, or dispatch. One finding, one proposal — and only for work
outside your ticket: what's in its scope is yours to do.
