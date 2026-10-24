# MADY development kit — operator entry points.
#
# The kit is the fleet: one self-ticking daemon (fleetd) + its client (fleetc).
# `install.sh` gates the Brewfile, stows agents/skills (+ the .claude config), and
# creates the runtime home `~/.fleet/` (fleetd writes ledger.db/fleetd.sock/
# telemetry.db there); `cargo` builds the binaries. Run from the repo: `make <target>`.
#
# `make` with no target prints the help summary.

.DEFAULT_GOAL := help
.PHONY: help install uninstall run stop dashboard

help:                            ## Show this help
	@awk 'BEGIN {FS = ":.*?## "} /^[a-zA-Z_-]+:.*?## / { printf "  \033[36m%-12s\033[0m %s\n", $$1, $$2 }' $(MAKEFILE_LIST)

install:                         ## Install the kit (Brewfile + stow) and fleetc onto PATH
	./install.sh
	cargo install --path crates/fleetc --force

uninstall:                       ## Uninstall the kit and remove fleetc
	-cargo uninstall fleetc
	./install.sh --uninstall

run:                             ## Build + run the fleet daemon in the foreground (open a pane first; Ctrl-C to stop)
	@if pgrep -x fleetd >/dev/null; then echo "fleetd already running — make stop first"; exit 1; fi
	FLEET_HUB=$(CURDIR)/worktrees/master FLEET_CONFIG=$(CURDIR)/fleet.toml cargo run -p fleetd

stop:                            ## Stop the fleet daemon
	@pkill -x fleetd && echo "fleetd stopped" || echo "fleetd not running"

dashboard:                       ## Build + run the TUI dashboard (cargo run --release)
	cargo run --release -p dashboard
