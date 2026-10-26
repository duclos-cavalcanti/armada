---
title: Document the crate's functions in the README
type: task
status: todo
blocked_by:
- ARMT-4
---
## Outcome

The README lists each public function (`add`, `sub`, `neg`) with a one-line example.

## Why

A reader of the sandbox sees what it offers without opening `src/lib.rs`. Blocked by ARMT-4,
so `neg` exists to document.

## Acceptance criteria

- The README has a "Functions" section naming `add`, `sub` and `neg`, each with an example.

## Out of scope

- Generated API docs (`cargo doc`).
