# Armada

A self-contained development armada — an always-on **fleet** — that turns 
tickets into merged code for a given project. Work is designed, produced,
and verified by a crew of agent **cogs**, coordinated by a daemon (`fleetd`) that
owns a SQLite **ledger** (the armada's state).

**`fleetc` is the interface to the running armada** — how you talk to, direct,
and query it: dispatch work, open convoys, anchor/weigh/sink cogs, and read its
plan, cogs, transcripts, and cost. `fleetd` owns the state and drives the line;
`fleetc` is the control surface.

Forward backlog: [`TODO.md`](TODO.md).

![Fleet overview — operator → fleetc → fleetd → crew → project, with the skills, external systems, ledger and dashboard](demos/assets/fleet-overview.png)

---

## Getting started

### 1. Dependencies

Homebrew is the install path for every prerequisite; the `Brewfile` is the
canonical list. The installer gates on it.

```bash
# 1. Install Homebrew (skip if present): https://brew.sh/
# 2. Install every Brewfile entry (idempotent):
brew bundle --file=./Brewfile

# 3. Set up a project — a folder projects/<name>/ holding its fleet.toml (forge, tracker,
#    cadences) and its checkout. `/fleet-new-project` (in Claude, from the repo root) asks
#    what it needs, writes the folder, and runs preflight; the tracked showcase
#    projects/armada-test/ is the worked example of every field.

# 4. Third-party backends live in third-party/
#    Keep current: re-clone, or `git -C third-party/<repo> pull`.
```

Integrations are chosen in `fleet.toml` (`[forge]`, `[tracker]`); initialize each CLI once:

- **Forges:** GitLab — `glab auth login`. GitHub — `gh auth login`; enable "Allow auto-merge" and
  protect the default branch with your CI check. GitHub has no MR dependencies, so it doesn't stack.
- **Trackers:** Jira — `jira init` (its board is the one claimed tickets join the active
  sprint of). Filesystem — no setup: a folder of `<KEY>.md` ticket files with a YAML header
  (`[tracker] path`, default `tickets/` in the project folder).

### 2. Install

```bash
make install      # gate on the Brewfile, stow the Claude surface, put fleetc on PATH
make uninstall    # reverse it (unstow; assumes the fleet is stopped)
```

`make install` uses [stow](https://www.gnu.org/software/stow/) to symlink the
agents and skills into place.

Per-deployment harness settings (auto-mode trust, permissions for *your* work or
personal systems) live in a gitignored profile, applied onto `.claude/settings.json`:

```bash
harness/apply.sh harness/profiles/<name>.json   # rebuild = committed base + profile
```

The applied file is what `make install` stows into `~/.claude`. The committed base
stays frozen: `apply.sh` marks the file `skip-worktree`, so git ignores the applied
copy. To un-apply (or to edit the base itself), clear the flag first:

```bash
git update-index --no-skip-worktree .claude/settings.json
git checkout .claude/settings.json
```

### 3. Run

```bash
make preflight P=<name>   # check the setup (runtime state, checkout, CLI auth, tmux + claude); `make run` gates on it
make run P=<name>         # start project <name>'s fleetd in the background (unless it's up), then open its dashboard
make stop P=<name>        # stop that project's fleet daemon
make dashboard P=<name>   # build + run the TUI dashboard
make admiral P=<name>     # talk to the project's admiral (its product expert; reads the kit, never edits it)
make export P=<name>      # snapshot the project's ledger → projects/<name>/exports/<name>-<UTC stamp>.db
```

`fleetd` self-ticks (poll / sync / reconcile cadences, set in `fleet.toml`);
`fleetc` is the control surface — run `fleetc` for its verbs.

**A project is a folder.** `projects/<name>/` holds the project's `fleet.toml`, its runtime state
in `.fleet/` (ledger, socket, telemetry, `fleetd.log`), its checkout, and its ticket worktrees. `fleetd`,
`fleetc`, the dashboard and every cog find the project as the nearest folder with a `fleet.toml`,
starting from the current directory, or from `-C <dir>` (`fleetc -C projects/<name> …`). Outside a
project they refuse to run. One fleet runs at a time. To switch, let the cogs finish (or
`fleetc sink` them), `make stop`, then `make run P=<other>`. Each project's state stays in its
folder, so you come back to it exactly as you left it.

`.fleet/` is live SQLite; to keep or move a project's state, snapshot its ledger with
`fleetc export <file>` (a consistent copy, safe beside a running fleet) and restore it with
`fleetc import <file>` (with the fleet stopped).

---

## Layout

```
armada/
├── crates/      the fleet — domain, ledger, wire, fleetd (daemon), fleetc (client)
├── apps/        standalone TUIs — crew, dashboard
├── agents/      the cogs + standing collaborators, stowed to ~/.claude/agents/
├── skills/      the kit's skills, stowed to ~/.claude/skills/
├── .claude/     Claude config (settings, CLAUDE.md, scripts), stowed to ~/.claude/
├── harness/     apply.sh + gitignored per-deployment settings profiles
├── demos/       architecture decks (open-slide)
├── Brewfile     dependency manifest
├── install.sh   install / --uninstall (gates on Brewfile + stows)
├── Makefile     one-word wrappers (run `make` for the cheat sheet)
├── TODO.md      the forward backlog
├── KNOWN_ISSUES.md  accepted edge cases + workarounds
└── projects/    one folder per project: fleet.toml + .fleet/ state + checkout + ticket worktrees
                 (gitignored, except the armada-test showcase's config + tickets)
```
