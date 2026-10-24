# Brewfile — MADY development kit prerequisites.
#
#   $ brew bundle --file=Brewfile
#
# Covers the skill stowing, the launchd-driven tick runtime under
# tooling/mady/, and the day-to-day GitLab / Jira / tmux / AWS workflow the
# skills rely on. Anything not on Homebrew is listed in comments below
# with an install pointer.

cask "claude-code@latest"   # auto-updating variant — plain "claude-code" is a distinct, pinned cask
brew "pi-coding-agent"

# --- Core: stow + shell tools ---
brew "stow"            # symlink farm manager — used by install.sh
brew "tmux"            # terminal multiplexer — substrate for launch-session.sh
brew "jq"              # JSON wrangling in tick scripts + skill bodies
brew "yq"              # YAML wrangling (CDK / GitLab CI / settings.json edits)
brew "bats-core"       # Bash test runner (Bats) — `make test` runs bin/test/*.bats
                         # (exit 3 on schema drift) and ad-hoc validation against
                         # tooling/mady/schemas/*.json.

# --- GitLab / Jira ---
brew "glab"            # GitLab CLI — MR finish, pipeline polling, threads
brew "jira-cli"        # Jira CLI (ankitpokhrel/jira-cli) — reads a ticket's type/summary
                       # for branch naming (fleet-berth), and a fallback when the
                       # Atlassian MCP isn't bound (`claude --print` / cron / shell).

# --- AWS (CDK synth, ephemeral stacks, Lambda surfaces) ---
brew "awscli"          # core AWS CLI — used by /ephemeral and CDK workflows
brew "aws-sso-util"    # SSO helper — `aws configure sso` + automatic credential refresh

# --- Rust build-cache hygiene ---
brew "cargo-sweep"     # trims stale artifacts from the shared cargo target/ cache —
                       # bin/clean.sh's sweep phase (clean_sweep) SKIPs without it.
                       # Brew-installed so cron.sh's PATH fix (/opt/homebrew/bin) reaches it.

# --- Demos (open-slide presentation decks under demos/) ---
brew "node"            # Node.js + npm/npx — builds & serves the open-slide decks
                       # (`cd demos && npm install && npm run dev`); also the tools/viz app.

# --- macOS launchd extras ---
brew "terminal-notifier"  # clickable toasts from launchd ticks (escalations)

# --- Optional analytics (mirrors .claude-otel/ recipe) ---
# brew "duckdb"        # Parquet + SQL for usage / cost rollups

# --- Not on Homebrew — install separately ---
# (none currently — the kit's Jira CLI need is met by `jira-cli` above.
#  Atlassian's official `acli` is an alternative but not preferred —
#  ankitpokhrel/jira-cli is more mature for headless / scripted use.)
