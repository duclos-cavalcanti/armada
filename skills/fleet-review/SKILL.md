---
description: Review one MR, ours or an ally's, against its ticket (if ours), /fleet-ready-done's Done and the project's own doctrine, and post the findings on it — on our MR each must-fix and should-fix as its own resolvable thread, on an ally's one summary comment. Returns `VERDICT: clean | changes`. Never approves. The kit's default review; a project names its own in fleet.toml (`review_skill`). For the lookout.
argument-hint: [<iid>] (default: FLEET_SHIP)
---

# Skill: fleet-review

You review **one** MR, independently, on its evidence: its diff, its ticket's ACs (ours only), the
project's doctrine and `/fleet-ready-done`'s Done. You **diagnose; you never edit**: your Bash is
for reading (`git diff`, `git show`, `grep`, running the tests), never for changing the tree.

## Ground first

```bash
SHIP=$(fleetc ship)   # FLEET_SHIP's MR: {ship: {ticket, target, …}, snapshot: {head_sha, title, …}, fetch_ref, ours}
git fetch origin "$(printf '%s' "$SHIP" | jq -r .fetch_ref)" "$(printf '%s' "$SHIP" | jq -r .ship.target)" --quiet
BASE="origin/$(printf '%s' "$SHIP" | jq -r .ship.target)"
HEAD_SHA=$(printf '%s' "$SHIP" | jq -r .snapshot.head_sha)
git diff "$BASE...$HEAD_SHA" --stat   # what changed
git diff "$BASE...$HEAD_SHA"          # how
```

- **Ours** (`.ours` is true): `fleetc ticket <KEY>` (`.ship.ticket`). Its acceptance criteria are
  the bar.
- **An ally's**: no ticket of ours. The bar is the project's doctrine plus the MR's stated
  purpose, its title and description read as claims to check, not premises.

Then read the project's doctrine for the area the diff touches (`CLAUDE.md`, the `docs/` it
cites, the conventions near the changed code), and `/fleet-ready-done`'s Done. Run the tests when
a finding depends on whether they pass.

**Evidence, not narrative.** Commit messages, the MR description and code comments are the
author's *claims*. Check them against the diff; never take them as premises.

**A later round** (`fleetc review` lists the MR's threads): judge the property, not the remedy. A
finding is cleared when the violated property now holds, by whatever route the author chose. A
thread the author answered with evidence stays answered unless that evidence is wrong; say where.
A new defect in reworked code is a new finding with its own evidence.

## Review

Judge the diff on:

- **Acceptance criteria** (ours): is each one met, and proven by a test or a stated verification?
- **Correctness**: bugs, logic errors, unhandled cases, unsafe assumptions.
- **Design**: the project's layering and conventions; the change sits where it belongs.
- **Security**: input validation, secrets, permissions, injection.
- **Tests**: the behaviour the change adds is tested, in this change.
- **Scope**: the change does what its ticket says, and nothing it doesn't.

Each finding is:

```
[<severity>] <file>:<line> — <the violated property or desired outcome>
  evidence: <the diff line, quoted snippet, or doctrine you're citing>
  also:     <every other site of the same defect in the diff, or omit>
  impact:   <what goes wrong if it ships>
  candidate fix (non-binding): <one approach the author may take or ignore>
```

One finding per defect, with every site it reaches, never one finding per site. Offer a candidate
fix only once you've checked it against the same ground truth as the finding; otherwise leave it
out.

Severities:

- **must-fix**: a bug, a security hole, a data-integrity risk, an unmet acceptance criterion.
- **should-fix**: a missing test, a design or convention deviation, a maintainability hazard.
- **nit**: style, naming, polish. Never blocks.

Don't invent problems. A category with nothing in it gets one line saying so.

`clean` means no must-fix and no should-fix remain (nits may).

## Post

- **Ours**: each must-fix and should-fix as its own thread, which its worker answers and
  resolves; then one summary comment with the verdict and the nits:
  ```bash
  fleetc review-post --thread --body - <<'EOF'
  <one finding>
  EOF
  fleetc review-post --body - <<'EOF'
  VERDICT: clean | changes
  <nits, or "no nits">
  EOF
  ```
- **An ally's**: one summary comment, the verdict and every finding, most severe first:
  ```bash
  fleetc review-post --body - <<'EOF'
  VERDICT: clean | changes
  <findings>
  EOF
  ```

Never `--approve`: approving is the lookout's call, after its crossing check. Then return the
`VERDICT: clean | changes` line to your caller.

**Without an MR** (a hand's review on request, given a base, `HEAD` and maybe a ticket): ground
on those refs instead of `fleetc ship`, review the same way, and return the verdict and findings,
most severe first. Post nothing.
