---
title: Add neg to the sandbox crate
type: story
status: todo
---
## Outcome

The crate exposes `neg(a)`, returning `-a`.

## Why

The sandbox needs a small, real change to drive the fleet through a full cycle: ticket →
worker → PR → sailor → merge.

## Acceptance criteria

- `neg(5) == -5` and `neg(-5) == 5`, each asserted by a unit test.
- `cargo test` passes.

## Out of scope

- Overflow handling for `i64::MIN`: the sandbox only exercises the fleet, not arithmetic.
