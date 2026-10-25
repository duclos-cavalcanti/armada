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

# 3. Clone the project to work on — `[project] path` in fleet.toml (default worktrees/master).
git clone git@gitlab.com:<project> worktrees/master

# 4. Third-party backends live in third-party/
#    Keep current: re-clone, or `git -C third-party/<repo> pull`.
```

Integrations are chosen in `fleet.toml` (`[forge]`, `[tracker]`); initialize each CLI once:

- **Forges:** GitLab — `glab auth login`. GitHub — `gh auth login`; enable "Allow auto-merge" and
  protect the default branch with your CI check. GitHub has no MR dependencies, so it doesn't stack.
- **Trackers:** Jira — `jira init` (its board is the one claimed tickets join the active
  sprint of). Filesystem — planned.

### 2. Install

```bash
make install      # gate on the Brewfile, stow the Claude surface, put fleetc on PATH
make uninstall    # reverse it (unstow; assumes the fleet is stopped)
```

`make install` uses [stow](https://www.gnu.org/software/stow/) to symlink the
agents, skills, and `~/.fleet` runtime into place.

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
make preflight    # check the setup (project checkout, CLI auth, tmux + claude); `make run` gates on it
make run          # build + run fleetd in the foreground (open a tmux pane first; Ctrl-C stops)
make stop         # stop the fleet daemon
make dashboard    # build + run the TUI dashboard
```

`fleetd` self-ticks (poll / sync / reconcile cadences, set in `fleet.toml`);
`fleetc` is the control surface — run `fleetc` for its verbs.

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
├── fleet.toml   fleetd config (cadences, dry_run, [project], [forge], [tracker])
├── TODO.md      the forward backlog
├── KNOWN_ISSUES.md  accepted edge cases + workarounds
└── worktrees/   product overlay — project checkout + ticket worktrees (gitignored)
```
