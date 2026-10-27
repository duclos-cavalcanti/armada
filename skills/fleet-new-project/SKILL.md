---
description: >-
  Set up a new fleet project: grill the operator on what fleet.toml needs (forge, tracker,
  checkout, merge policy), then create projects/<name>/ with its fleet.toml, its tickets/ folder
  (filesystem tracker), and its checkout, and run preflight until it's green. Run from the
  armada repo root. The repeatable way to add a project.
argument-hint: "[name]"
---

# Skill: fleet-new-project

You are adding a project to the fleet. A project is a folder, `projects/<name>/`: its
`fleet.toml`, its checkout, its ticket worktrees, and its runtime state in `.fleet/` (fleetd
creates that). Your job: ask what you need, write the folder, prove it with preflight.

Run from the **armada repo root** (there's no project yet, so `fleetc` can't help). The
tracked showcase `projects/armada-test/fleet.toml` is the worked example of every field.

## Step 1 — Grill the operator

Ask in small groups and confirm each answer before moving on; never guess a value. Validate
as you go.

1. **Name** — the folder name (`projects/<name>`): lowercase, `[a-z0-9-]`. It must not exist yet.
2. **Forge** — where the code lives:
   - `github`: `owner/repo`. Check it: `gh repo view <owner/repo>` (and `gh auth status`).
   - `gitlab`: the numeric project id. Check it: `glab api projects/<id>` (and `glab auth status`).
   - Does the repo exist yet? If not, ask whether to create it (`gh repo create … --private`,
     with CI and branch protection like `armada-test`) or stop until they've made it.
   - For GitHub: does the repo allow auto-merge, and does the default branch require a CI
     check? (`gh api repos/<owner/repo>` → `allow_auto_merge`; preflight warns on both.)
3. **Tracker** — where tickets live:
   - `filesystem`: the ticket key prefix (`[A-Z][A-Z0-9]*`, e.g. `PRAC`), the folder (default
     `tickets`), and who claims tickets (default: `git config user.name`).
   - `jira`: the Jira project key (the prefix), an optional claim label, and `jira init` done
     (`jira me` answers).
4. **Checkout** — the folder name for the clone (default: the repo's default branch, e.g.
   `main`), and the clone URL (`git@github.com:<owner/repo>.git`, or the GitLab SSH URL).
5. **Merge policy** — `auto_merge`: should the sailor arm auto-merge (true), or land each MR
   to clean and wait for the operator to merge (false)?
6. **Anything else** — keep the default cadences and `dry_run = false` unless they say
   otherwise.

Read the answers back as the fleet.toml you'll write, and get a yes before Step 2.

## Step 2 — Create the folder

```bash
NAME=<name>
mkdir -p "projects/$NAME"
# filesystem tracker only:
mkdir -p "projects/$NAME/tickets"
git clone "<clone-url>" "projects/$NAME/<checkout>"
git -C "projects/$NAME/<checkout>" symbolic-ref --short refs/remotes/origin/HEAD   # the default branch is known
```

Write `projects/$NAME/fleet.toml` in the showcase's shape — a short header saying what the
project is, then:

```toml
poll_secs      = 5
sync_secs      = 300
reconcile_secs = 60
dry_run        = false

[project]
path = "<checkout>"            # relative to this folder

[forge]
kind       = "<github|gitlab>"
project    = "<owner/repo or id>"
auto_merge = <true|false>

[tracker]
kind    = "<filesystem|jira>"
project = "<PREFIX>"
# filesystem: path = "tickets", user = "…"  ·  jira: claim_label = "…"
```

`projects/` is gitignored (only the showcase is tracked), so the new project stays local;
whether to version it is the operator's call.

## Step 3 — Preflight until green

```bash
make preflight FLEET_PROJECT=$NAME
```

Every line must be ✓. For a ✗, fix it with the operator (a CLI to log into, a repo setting, a
clone that didn't land) and re-run. Then tell them how to start it:

```bash
make run FLEET_PROJECT=$NAME        # in a tmux pane
```

And remind them: if the harness profile's auto-mode context doesn't yet name this repo as
trusted infrastructure, add it to their profile (`harness/profiles/<name>.json`) and re-apply
it themselves (`harness/apply.sh …`) — the cogs' pushes and PRs may be blocked otherwise.

## Boundaries

You create the project folder and nothing else: no tickets (that's `/fleet-commission`), no
dispatch, no changes to the armada repo or to the harness profile. Never overwrite an existing
`projects/<name>`.
