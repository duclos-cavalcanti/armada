---
description: Rebase the current branch's MR against its target. Clean divergence → trigger GitLab's server-side rebase via `fleetc rebase` and exit (the sailor re-fires on the result). Conflict → resolve locally with project/code understanding and force-push. Escalate only when the conflict needs a product/design decision.
argument-hint: [MR_IID]
---

# Skill: fleet-rechart

Rebase the current branch's open MR against its target branch. The decision turns
on **whether the MR is conflicted**:

- **Not conflicted** (behind target / `need_rebase`) → the rebase is mechanical:
  trigger GitLab's server-side rebase (`fleetc rebase`) and exit. You do **not**
  wait for it — the sailor re-fires when the result lands (the rebase moves
  `head_sha`, which the fleet's fingerprint watches).
- **Conflicted** → a server-side rebase can't resolve it (GitLab aborts). This is
  the one part that needs judgment — **resolving a real conflict using
  understanding of the project's code** — so do it locally and force-push.

## Contract

- **Clean path:** triggers the server rebase and exits. The outcome — a clean
  `head_sha` advance, or a surprise `conflict` that flips `has_conflicts` — is
  observed on the sailor's **next sweep**, not synchronously. Both perturb the
  fingerprint, so the sailor re-fires and re-decides.
- **Conflict path:** makes a resolution commit and force-pushes — which, with the
  project's *remove-approvals-on-push* setting, drops the MR below its approval
  threshold, so the resolved diff is **re-reviewed before it can merge**. The
  force-push advances `head_sha` → re-fire.
- An undecidable conflict is an **escalation, not a force-push** (exit 1).

## Steps

### 1 — Resolve the MR IID

Use the passed argument if given; otherwise discover from the branch:

```bash
MR_IID="${ARG_IID:-$(glab mr list --source-branch "$(git branch --show-current)" --output json | jq -r '.[0].iid // empty')}"
[ -n "$MR_IID" ] || { echo "no open MR for this branch" >&2; exit 1; }
```

### 2 — Branch on conflict state

Read GitLab's merge-readiness fresh and split clean-vs-conflict:

```bash
MR=$(glab api "projects/:fullpath/merge_requests/$MR_IID")
CONFLICTED=$(printf '%s' "$MR" | jq -r 'if (.has_conflicts==true) or (.detailed_merge_status=="conflict") then "true" else "false" end')
TARGET=$(printf '%s' "$MR" | jq -r '.target_branch')

if [ "$CONFLICTED" = "false" ]; then
  # Clean divergence — trigger the server-side rebase (disarms MWPS if needed) and exit.
  # Don't wait: the sailor re-fires when head_sha moves (or has_conflicts flips, on a surprise).
  fleetc rebase --iid "$MR_IID"
  exit 0
fi
# Conflicted → resolve locally (Step 3).
```

### 3 — Intelligent rebase (only when conflicted)

A server-side rebase can't proceed. Resolve it locally, on the merits:

```bash
git fetch origin "$TARGET"
git rebase "origin/$TARGET"
```

For each conflicting file (`git diff --name-only --diff-filter=U`):

- **Read the hunk and resolve it correctly** — understand what each side
  intended and produce the right merged code. NOT a blanket `--ours`/`--theirs`.
  This is the whole reason a script can't do this step.
- Edit the file, then **verify** with `git diff <file>` before `git add <file>`.
  One file at a time — never batch-resolve blindly.
- `git rebase --continue`. If a later commit conflicts, repeat.
- If `git rebase` stops on an **empty / already-applied** commit (common when
  unwinding a stacked MR onto the default branch — the parent's commits are already there,
  especially after a squash-merge), that is **not** a conflict to resolve: `git
  rebase --skip` it and continue. Resolving hunks only applies to genuine
  content conflicts.

On a clean resolution:

```bash
git push --force-with-lease
```

This publishes the resolved branch and resets approvals (re-review is forced).

### 4 — Self-reflection

Post a Jira comment: which path ran (clean-triggered / conflict-resolved), the
target branch, and — if you resolved a conflict — **which files and the gist of
how you resolved each**, so the re-reviewer knows exactly what to scrutinize.

## Escalate, don't guess

A conflict that hinges on intent you don't have — which of two divergent custody
behaviours is correct, a deliberate revert vs a re-add, an ambiguous policy
change — is an **escalation, not a force-push**. `git rebase --abort`, leave the
tree clean, exit 1. The sailor turns that into a `rebase_conflict` escalation
for a human. Force-pushing a guessed resolution is the one thing this skill must
never do.
