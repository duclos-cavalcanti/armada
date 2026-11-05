---
description: Take your bearings on the product — read its merged design canon (its design records, specs, conventions and Definition of Ready/Done, on `origin/HEAD`) and the code they point to, so you hold the current design and its open questions before a mission that needs the whole of it. For the admiral, on a grooming session; the operator runs it after launch.
argument-hint:
---

# Skill: fleet-bearings

You're about to work across the product's design, so get your bearings on all of it first. Read
the fleet's state only when a mission needs it.

1. **Fetch**, so `origin/HEAD` is current. The product is the project checkout:
   ```bash
   REPO="$(fleetc project)"
   git -C "$REPO" fetch origin --quiet
   ```
2. **Read the merged canon, never the working tree.** Start from its README / CLAUDE.md, which
   map the design docs: design records, specs, conventions, any Definition of Ready/Done.
   ```bash
   git -C "$REPO" show origin/HEAD:<path>
   git -C "$REPO" grep <pat> origin/HEAD
   ```
3. **Read the code they point to**, enough to know where each design lives in the build.

Then tell the operator, in a few lines, what you hold: the design's current shape, what's
ratified versus still open, and any drift you saw between the docs and the code. Await their
mission.
