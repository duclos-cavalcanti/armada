---
description: Launch the current branch's ship. Push it, draft the MR title + description from the ticket and the branch's divergence from its base, then open the MR via `fleetc launch` (or rewrite the ticket's MR, already open). fleetd's forge adapter opens it, on GitLab or GitHub alike. Run from the worktree.
argument-hint: [--draft]
---

# Skill: fleet-launch

You are launching a ship: opening the MR for the current branch. **The judgment is
yours:** a title and description written from the ticket and from what this branch
changes against its base. **The mechanics are fleetd's:** `fleetc launch` opens the MR
on whichever forge the fleet runs (target branch, assignee, squash), so never call
`glab` / `gh` here.

**Argument:** `--draft` (optional) opens it as a draft. The default is ready. A hand
iterating on a branch may want a draft; the handoff marks it ready later.

---

## Step 1 — Resolve the base + push

The base is the ticket's fork base (a stacked child forks off its parent's branch),
else origin's default branch. `fleetc launch` resolves the same base as the MR target.

```bash
BASE="$(fleetc packet 2>/dev/null | jq -r '.order.fork_from // empty')"   # empty for a ticketless hand
[ -n "$BASE" ] || BASE="$(git symbolic-ref --short refs/remotes/origin/HEAD)"
BASE="origin/${BASE#origin/}"
git fetch origin "${BASE#origin/}" --quiet --no-tags
git push -u origin HEAD
```

If the push is refused because the remote diverged (e.g. after a server-side rebase),
use `git push --force-with-lease`. Never plain `--force`: the lease refuses if someone
else pushed since your last fetch. Any other push failure (auth, network, protected
branch): surface it and stop. Don't retry blindly.

---

## Step 2 — Gather context

- **The ticket.** If you have one (`FLEET_TICKET`), `fleetc packet` carries its title
  (`.ticket.title`) and the acceptance-criteria summary (`.order.ac_summary`).
- **The change** against the base:
  ```bash
  git log "$BASE"...HEAD --oneline
  git diff "$BASE"...HEAD --stat
  git diff "$BASE"...HEAD
  ```

---

## Step 3 — Draft the title + description

**Title:** `<TICKET>: <what the change does>` (just the summary when there's no ticket).
The ticket key in the title links the MR to its ticket.

**Description:** written for a **future reader**, so focus on the *why*, not only the *what*:

```
## Summary

<2-4 sentences: the motivation and context. Why does this change exist? What problem does it solve? Reference the ticket.>

## Changes

<Bulleted list of the meaningful changes: components affected, new types/adapters, modified flows, infra changes.>

## Scope Decisions

<What was intentionally not addressed, and why.>

## Testing

<How it was tested: unit / integration tests added, manual verification, edge cases considered.>

🤖 Generated with [Claude Code](https://claude.com/claude-code)
```

---

## Step 4 — Launch

```bash
fleetc launch --title "<title>" --body "$(cat <<'EOF'
<the description from Step 3>
EOF
)"   # add --draft if asked
```

It prints the MR's URL. Show it. fleetd's next sync picks the ship up into the armada. If the
ticket's MR is already open (a rework), `fleetc launch` rewrites its title and description
instead of opening a second.

---

## Failure handling

If the push or `fleetc launch` fails in a way you can't resolve, report which step
failed, the error, and what you tried, in your response (the operator reads your
pane). Then stop. Never paper over a push failure with `--force`.
