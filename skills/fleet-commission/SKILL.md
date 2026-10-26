---
description: >-
  Commission a ship: file a new ticket that is Ready. Drafts the title and the
  four-section body from the conversation to /fleet-ready-done, checks it against
  Ready, wires its blockers and convoy, then creates it with `fleetc ticket new` on
  whichever tracker the project uses (Jira, the filesystem, …). Run from inside the
  project. For the admiral and the operator; files work, never dispatches it.
argument-hint: "[type]"
---

# Skill: fleet-commission

You are commissioning a ship — writing the ticket that orders a future change. **The
judgment is yours:** a title and body that meet `/fleet-ready-done`'s Ready. **The
mechanics are fleetd's:** `fleetc ticket new` creates it in the project's tracker, so
never call `jira` or write a ticket file by hand here.

**Argument:** `type` (optional) — `story`, `bug`, `task`, or `epic`. If omitted, pick the
one that fits per `/fleet-ready-done`, and say which.

## Step 1 — Draft

From the conversation (ask only for what's missing):

- **Title** — one action on a named thing.
- **Body** — `Outcome` / `Why` / `Acceptance criteria` / `Out of scope`, per
  `/fleet-ready-done`.

## Step 2 — Check it's new and wire it

- **Duplicates** — search the board for the title's key terms (filesystem tracker:
  `grep -ril "<term>" <tickets folder>`; Jira: its search). If an existing ticket already
  covers it, stop and name it instead of filing a second.
- **Blockers** — any existing ticket this one needs first is a `--blocked-by <KEY>`.
- **Convoy** — the epic it belongs to, if any, is `--convoy <KEY>`.

## Step 3 — Check Ready

Walk `/fleet-ready-done`'s Ready list against the draft. A gap (an untestable criterion,
two goals, too big for one MR) is fixed now — reshape it, or split it into several
commissions with their order as blockers. Don't file a ticket you'd have to hold.

## Step 4 — File

```bash
fleetc ticket new --type <type> --title "<title>" \
    ${CONVOY:+--convoy "$CONVOY"} --blocked-by <KEY> … --body - <<'EOF'
## Outcome
…
EOF
```

It prints the new key. Show the result with `fleetc ticket <KEY>`.

## Boundaries

You file the ticket; you don't assign, sprint, or dispatch it — that's the captain's
frontier or the operator's call. Never file guesswork: an unclear ask is a question back,
not a vague ticket.
