# Brewfile — armada prerequisites.
#
#   $ brew bundle --file=Brewfile
#
# Covers install.sh's stowing, the fleet's runtime (fleetd drives tmux, glab,
# gh, and Jira).
# Anything not on Homebrew is listed in comments below with an install pointer.

cask "claude-code@latest"   # auto-updating variant — plain "claude-code" is a distinct, pinned cask
brew "pi-coding-agent"

# --- Core: stow + shell tools ---
brew "stow"            # symlink farm manager — used by install.sh
brew "tmux"            # terminal multiplexer — fleetd hosts every cog in a tmux session
brew "jq"              # JSON wrangling in skill bodies + the statusline

# --- Forges + trackers (fleet.toml picks which) ---
brew "glab"            # GitLab CLI — MR finish, pipeline polling, threads
brew "gh"              # GitHub CLI — the GitHub forge adapter
brew "jira-cli"        # Jira CLI (ankitpokhrel/jira-cli) — fleetd's Jira tracker adapter
                       # shells out to it.

# --- Demos (open-slide presentation decks under demos/) ---
brew "node"            # Node.js + npm/npx — builds & serves the open-slide decks
                       # (`cd demos && npm install && npm run dev`); also the tools/viz app.

# --- Optional analytics (mirrors .claude-otel/ recipe) ---
# brew "duckdb"        # Parquet + SQL for usage / cost rollups

# --- Not on Homebrew — install separately ---
# (none currently — the kit's Jira CLI need is met by `jira-cli` above.
#  Atlassian's official `acli` is an alternative but not preferred —
#  ankitpokhrel/jira-cli is more mature for headless / scripted use.)
