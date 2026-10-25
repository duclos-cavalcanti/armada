# Armada — operator entry points.
#
# The kit is the fleet: one self-ticking daemon (fleetd) + its client (fleetc).
# `install.sh` gates the Brewfile and stows agents/skills (+ the .claude config); `cargo` builds
# the binaries. A project is a folder, projects/<name>/ (its fleet.toml + .fleet/ state + checkout):
# the fleet targets take P=<name>. Run from the repo: `make <target> P=<name>`.
#
# `make` with no target prints the help summary.

.DEFAULT_GOAL := help
.PHONY: help install uninstall need-project preflight run stop dashboard

help:                            ## Show this help
	@awk 'BEGIN {FS = ":.*?## "} /^[a-zA-Z_-]+:.*?## / { printf "  \033[36m%-12s\033[0m %s\n", $$1, $$2 }' $(MAKEFILE_LIST)

install:                         ## Install the kit (Brewfile + stow) and fleetc onto PATH
	./install.sh
	cargo install --path crates/fleetc --force

uninstall:                       ## Uninstall the kit and remove fleetc
	-cargo uninstall fleetc
	./install.sh --uninstall

# The fleet targets need P=<name> — the project folder projects/<name>/.
need-project:
	@if [ -z "$(P)" ]; then echo "P=<name> required — one of: $$(command ls projects)"; exit 1; fi

preflight: need-project          ## Check project P's setup: runtime state, checkout, forge + tracker CLIs, tmux + claude
	cargo run -q -p fleetd -- -C projects/$(P) preflight

run: need-project                ## Build + run project P's fleet daemon in the foreground (open a pane first; Ctrl-C to stop)
	@if pgrep -x fleetd >/dev/null; then echo "fleetd already running — make stop first"; exit 1; fi
	cargo run -p fleetd -- -C projects/$(P)

stop:                            ## Stop the fleet daemon
	@pkill -x fleetd && echo "fleetd stopped" || echo "fleetd not running"

dashboard: need-project          ## Build + run the TUI dashboard over project P (cargo run --release)
	cargo run --release -p dashboard -- -C projects/$(P)
