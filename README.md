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

---

## Getting started

### 1. Dependencies

Homebrew is the install path for every prerequisite; the `Brewfile` is the
canonical list. The installer gates on it.

```bash
# 1. Install Homebrew (skip if present): https://brew.sh/
# 2. Install every Brewfile entry (idempotent):
brew bundle --file=./Brewfile

# 3. The armada needs a master hub clone, which is the project to work on.
git clone git@gitlab.com:<project> worktrees/master

# 4. Third-party backends live in third-party/
#    Keep current: re-clone, or `git -C third-party/<repo> pull`.
```

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
git checkout .claude/settings.json              # un-apply
```

The applied file is what `make install` stows into `~/.claude` — don't commit it
while a profile is applied.

### 3. Run

```bash
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
├── .claude/     kit's Claude surface (agents + skills), stowed to ~/.claude/
├── demos/       architecture decks (open-slide)
├── Brewfile     dependency manifest
├── install.sh   install / --uninstall (gates on Brewfile + stows)
├── Makefile     one-word wrappers (run `make` for the cheat sheet)
├── fleet.toml   fleetd config (cadences, dry_run, gitlab_project)
├── TODO.md      the forward backlog
└── worktrees/   product overlay — master hub clone + ticket worktrees (gitignored)
```
