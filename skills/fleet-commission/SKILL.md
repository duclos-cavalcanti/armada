---
description: >-
  Commission a ship: file a new ticket that is Ready. Drafts it to /fleet-ready-done
  (title, four-section body, duplicate check, blockers + convoy, Ready check), then
  creates it with `fleetc ticket new` on whichever tracker the project uses (Jira, the
  filesystem, …). Run from inside the project. For the admiral and the operator — a
  worker, sailor or hand proposes instead (/fleet-propose). Files work, never dispatches it.
argument-hint: "[type]"
---

# Skill: fleet-commission

You are commissioning a ship — writing the ticket that orders a future change. **The
judgment is yours:** a ticket that meets `/fleet-ready-done`'s Ready. **The mechanics are
fleetd's:** `fleetc ticket new` creates it in the project's tracker, so never call `jira` or
write a ticket file by hand here.

**Argument:** `type` (optional) — `story`, `bug`, `task`, or `epic`. If omitted, pick the
one that fits per `/fleet-ready-done`, and say which.

## Step 1 — Draft

Follow `/fleet-ready-done`'s *Drafting a ticket*: draft, check it's new, wire it, check Ready.

## Step 2 — File

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
frontier or the operator's call. A cog (`FLEET_TICKET` set) can't file: `fleetc ticket new`
refuses it and points to `/fleet-propose`.
