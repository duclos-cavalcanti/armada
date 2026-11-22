---
title: "Tender: fleet-wide CI and bug fixes"
type: epic
status: todo
---
## Outcome

One standing home for the fixes the whole fleet waits on: a red that fails the same job the same
way across open PRs, fixed once, at its source, ahead of other work.

## Why

A fleet-wide red stalls every PR at once; here its fix is seated first, and every cog that meets
the same red finds it and waits on it instead of filing it again.

## Acceptance criteria

- Each ticket in it fixes one fleet-wide red; its title names the failing job and the error.
- It never closes.

## Out of scope

- A red one PR's own diff explains: that PR's sailor repairs it.
