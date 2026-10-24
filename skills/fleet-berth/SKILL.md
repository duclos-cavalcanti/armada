---
description: Create a git worktree + branch for a MADY ticket under the factory's worktrees/ dir, forked off a given base (default origin/master) of the master hub. Pure worktree creation — no Jira lifecycle. The operator or captain runs this when producing a dispatch, then passes the emitted path to `fleetc dispatch --worktree`; fleetd's reconcile spawns the cog there (reconcile never creates worktrees).
argument-hint: <ticket> [<fork-base-branch>]
---

# Skill: fleet-berth

You are creating the git worktree + branch for a MADY ticket so downstream work
can start. **This skill is Jira-read-only** — it fetches the ticket's issue-type
to derive the branch name, refreshes the fork base, then creates the worktree
under the factory's `worktrees/` dir, forked off the **master hub**. Jira-side
lifecycle (assign, label, sprint, transition) is NOT this skill's concern —
fleetd's spawn claims the ticket when it launches the cog.

**Argument shape:**

| Token | Required | Example | Effect |
|---|---|---|---|
| `<ticket>` | yes | `MADY-321` | The issue to create a worktree for |
| `<fork-base-branch>` | no (default `master`) | `chore/MADY-100-foo` | Branch to fork off. `master` for READY-CLEAN; the parent MR's source branch for a READY-STACKED dispatch, so the worktree builds on the parent's in-flight work rather than stale master. |

---

## Side-effect contract

This skill is **read-only against the hub's working tree and Jira**. Its only
side effects, in order:

1. **Git fetch** — `git -C "$REPO" fetch origin <base>` advances the hub's
   remote-tracking ref only (no master-fetch cron keeps it fresh anymore, so the
   skill refreshes it itself). Never touches the hub's working tree or any local
   branch.
2. **Git worktree** — `git worktree add` creates a NEW directory + branch under
   the factory's `worktrees/` (beside the `master` hub). The hub's files are not
   touched, and no commit is made on any existing branch.
3. **Shared-cache symlinks** — plants the target repo's declared
   `worktree.symlinkDirectories` symlinks (`target/`, the CDK venv) **into the
   NEW worktree only**, pointing at the **master hub**, so every worktree shares
   one build cache instead of rebuilding its own (tens of GB). Touches only the
   freshly-created worktree, never the hub.
4. **Stdout** — emits the new worktree path + branch.

**This skill never calls `Edit` / `Write` / `MultiEdit` against a working tree,
and never writes to Jira.** If you find yourself reaching for those, the skill is
off-path — stop and reconsider.

---

## Step 0 — Resolve names + fork base, create the worktree

Run these in the **same Bash invocation** (so `$REPO` is in scope for the git
calls):

1. Resolve the **master hub** — the factory's canonical checkout you fork ticket
   worktrees off (`$FLEET_HUB`, the same hub fleetd uses: `…/worktrees/master`).
   Fail closed if it's unset rather than forking off the wrong tree:
   ```bash
   REPO="${FLEET_HUB:?set FLEET_HUB to the master hub (…/worktrees/master)}"
   ```
2. Fetch the ticket's issue type + summary (via the `jira` CLI):
   ```bash
   jira issue view <ticket> --raw | jq '.fields | {issuetype: .issuetype.name, summary}'
   ```
3. Derive the names:
   - **prefix** from issue type: `Story` → `feature`, `Bug` → `fix`, everything
     else (`Task`, `Sub-task`, …) → `chore`.
   - **slug** — a kebab-case condensation of the summary (3–5 words).
   - **branch** = `<prefix>/<ticket>-<slug>`.
   - **path** — under the factory's `worktrees/` (beside the `master` hub), named
     `<branch-with-"/"-as-"-">`. E.g. hub `…/worktrees/master` + branch
     `chore/MADY-944-block-hmac-key` → `…/worktrees/chore-MADY-944-block-hmac-key`:
     ```bash
     LEAF="<prefix>-<ticket>-<slug>"        # branch with "/" → "-"
     WORKTREE_PATH="$(dirname "$REPO")/$LEAF"
     ```
4. Refresh the fork base + create the worktree off the hub (base = the 2nd arg,
   default `master`):
   ```bash
   BASE="<fork-base-branch>"               # default: master
   git -C "$REPO" fetch origin "$BASE" --quiet --no-tags
   git -C "$REPO" worktree add -b "<branch>" "$WORKTREE_PATH" "origin/$BASE"
   ```
5. **Plant the shared-cache symlinks** (into the new worktree, pointing at the
   hub). The factory cuts worktrees autonomously and in volume; without these,
   every worktree rebuilds its own `target/` (tens of GB) and CDK venv. The hub
   is a plain clone with no `post-checkout` hook, so the kit **owns** this step.
   The dir list is the target repo's own declaration in its `.claude/settings.json`
   (`worktree.symlinkDirectories` — single source of truth; don't hardcode it):
   ```bash
   while IFS= read -r rel; do
       [ -n "$rel" ] || continue
       src="$REPO/$rel"; dst="$WORKTREE_PATH/$rel"
       [ -e "$src" ] || continue        # hub hasn't materialised it yet (e.g. .venv pre-first-build)
       [ -e "$dst" ] && continue        # already present — never clobber
       mkdir -p "$(dirname "$dst")" && ln -s "$src" "$dst" && echo "linked $rel -> $src"
   done < <(jq -r '.worktree.symlinkDirectories[]? // empty' "$REPO/.claude/settings.json" 2>/dev/null)
   ```
   A `target/` symlink that never appears (empty `symlinkDirectories`, or the hub
   hasn't built yet) means the worker will cold-build — warm the hub once
   (`cargo build` / CDK bootstrap) so worktrees inherit the cache.

---

## Step 1 — Hand-off

Print, on stdout, so the caller (the operator/captain producing the dispatch) can
capture them and pass `worktree_path` to `fleetc dispatch`:

```
WORKTREE_PATH=<absolute-path>
BRANCH=<prefix>/<ticket>-<slug>
```

---

## Checklist

- [ ] Fork base fetched fresh (`git -C "$REPO" fetch origin <base>`) before the
  worktree was cut
- [ ] Worktree created under `worktrees/<leaf>` (beside the `master` hub); branch
  derived from issue type; forked off `origin/<base>`
- [ ] Shared-cache symlinks planted in the new worktree (`target/` etc.) → the
  hub, per the target repo's `worktree.symlinkDirectories`
- [ ] No Jira writes performed (assign / label / sprint / transition all skipped
  — fleetd's spawn claims the ticket)
- [ ] `WORKTREE_PATH=` and `BRANCH=` emitted on stdout
