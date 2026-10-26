---
description: Create a git worktree + branch for a ticket in its project folder (beside the project checkout), forked off a given base (default: origin's default branch) of the project checkout, which it locates via `fleetc project` — run it from inside the project (or its checkout / a worktree). Pure worktree creation — no ticket lifecycle. The operator or captain runs this when producing a dispatch, then passes the emitted path to `fleetc dispatch --worktree`; fleetd's reconcile spawns the cog there (reconcile never creates worktrees).
argument-hint: <ticket> [<fork-base-branch>]
---

# Skill: fleet-berth

You are creating the git worktree + branch for a ticket so downstream work
can start. **This skill only reads the ticket** — it fetches the ticket's type
to derive the branch name, refreshes the fork base, then creates the worktree
in the project folder, beside the **project checkout** it forks off. Tracker-side
lifecycle (assign, label, sprint, transition) is NOT this skill's concern —
fleetd's spawn claims the ticket when it launches the cog.

**Argument shape:**

| Token | Required | Example | Effect |
|---|---|---|---|
| `<ticket>` | yes | `PROJ-321` | The issue to create a worktree for |
| `<fork-base-branch>` | no (default: origin's default branch) | `chore/PROJ-100-foo` | Branch to fork off. The default branch for READY-CLEAN; the parent MR's source branch for a READY-STACKED dispatch, so the worktree builds on the parent's in-flight work rather than the stale default branch. |

---

## Side-effect contract

This skill is **read-only against the project checkout's working tree and the tracker**. Its only
side effects, in order:

1. **Git fetch** — `git -C "$REPO" fetch origin <base>` advances the project checkout's
   remote-tracking ref only (no fetch cron keeps it fresh anymore, so the
   skill refreshes it itself). Never touches the checkout's working tree or any local
   branch.
2. **Git worktree** — `git worktree add` creates a NEW directory + branch under
   the project folder (beside the project checkout). Its files are not
   touched, and no commit is made on any existing branch.
3. **Shared-cache symlinks** — plants the target repo's declared
   `worktree.symlinkDirectories` symlinks (e.g. `target/`, a virtualenv) **into the
   NEW worktree only**, pointing at the **project checkout**, so every worktree shares
   one build cache instead of rebuilding its own (tens of GB). Touches only the
   freshly-created worktree, never the checkout.
4. **Stdout** — emits the new worktree path + branch.

**This skill never calls `Edit` / `Write` / `MultiEdit` against a working tree,
and never writes to the tracker.** If you find yourself reaching for those, the skill is
off-path — stop and reconsider.

---

## Step 0 — Resolve names + fork base, create the worktree

Run these in the **same Bash invocation** (so `$REPO` is in scope for the git
calls):

1. Resolve the **project checkout** — the main clone you fork ticket worktrees off.
   fleetd knows it (`[project] path` in fleet.toml), so ask it — this works from any
   directory. Fail closed if fleetd is unreachable rather than fork off the wrong tree:
   ```bash
   REPO="$(fleetc project)" || { echo "fleetd unreachable — start it (make run)" >&2; exit 1; }
   ```
2. Read the ticket's type + title (its first two lines), from whichever tracker the
   project uses:
   ```bash
   fleetc ticket <ticket> | head -2    # "<KEY> · <type> · <status>" then the title
   ```
3. Derive the names:
   - **prefix** from the type: `story` → `feature`, `bug` → `fix`, everything
     else (`task`, `sub-task`, …) → `chore`.
   - **slug** — a kebab-case condensation of the summary (3–5 words).
   - **branch** = `<prefix>/<ticket>-<slug>`.
   - **path** — in the project folder (beside the project checkout), named
     `<branch-with-"/"-as-"-">`. E.g. checkout `projects/p/master` + branch
     `chore/PROJ-944-block-hmac-key` → `projects/p/chore-PROJ-944-block-hmac-key`:
     ```bash
     LEAF="<prefix>-<ticket>-<slug>"        # branch with "/" → "-"
     WORKTREE_PATH="$(dirname "$REPO")/$LEAF"
     ```
4. Refresh the fork base + create the worktree off the checkout (base = the 2nd arg,
   default: origin's default branch, which `git clone` recorded as `origin/HEAD`):
   ```bash
   BASE="<fork-base-branch>"               # empty → origin's default branch
   [ -n "$BASE" ] || BASE="$(git -C "$REPO" symbolic-ref --short refs/remotes/origin/HEAD)"
   BASE="${BASE#origin/}"
   git -C "$REPO" fetch origin "$BASE" --quiet --no-tags
   git -C "$REPO" worktree add -b "<branch>" "$WORKTREE_PATH" "origin/$BASE"
   ```
5. **Plant the shared-cache symlinks** (into the new worktree, pointing at the
   checkout). The factory cuts worktrees autonomously and in volume; without these,
   every worktree rebuilds its own build caches (`target/` alone can be tens of GB). The checkout
   is a plain clone with no `post-checkout` hook, so the kit **owns** this step.
   The dir list is the target repo's own declaration in its `.claude/settings.json`
   (`worktree.symlinkDirectories` — single source of truth; don't hardcode it):
   ```bash
   while IFS= read -r rel; do
       [ -n "$rel" ] || continue
       src="$REPO/$rel"; dst="$WORKTREE_PATH/$rel"
       [ -e "$src" ] || continue        # checkout hasn't materialised it yet (e.g. .venv pre-first-build)
       [ -e "$dst" ] && continue        # already present — never clobber
       mkdir -p "$(dirname "$dst")" && ln -s "$src" "$dst" && echo "linked $rel -> $src"
   done < <(jq -r '.worktree.symlinkDirectories[]? // empty' "$REPO/.claude/settings.json" 2>/dev/null)
   ```
   A `target/` symlink that never appears (empty `symlinkDirectories`, or the checkout
   hasn't built yet) means the worker will cold-build — warm the checkout once
   (e.g. `cargo build`) so worktrees inherit the cache.

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
- [ ] Worktree created at `projects/<p>/<leaf>` (beside the project checkout); branch
  derived from issue type; forked off `origin/<base>`
- [ ] Shared-cache symlinks planted in the new worktree (`target/` etc.) → the
  checkout, per the target repo's `worktree.symlinkDirectories`
- [ ] No tracker writes performed (assign / label / sprint / transition all skipped
  — fleetd's spawn claims the ticket)
- [ ] `WORKTREE_PATH=` and `BRANCH=` emitted on stdout
