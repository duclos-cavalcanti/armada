# Armada — operator entry points.
#
# The kit is the fleet: one self-ticking daemon (fleetd) + its client (fleetc).
# `install.sh` gates the Brewfile and stows agents/skills (+ the .claude config); `cargo` builds
# the binaries. A project is a folder, projects/<name>/ (its fleet.toml + .fleet/ state + checkout):
# every fleet target takes P=<name> (only help / install / uninstall don't). Run from the repo:
# `make <target> P=<name>`.
#
# `make` with no target prints the help summary.

.DEFAULT_GOAL := help
.PHONY: help install uninstall .project preflight run stop dashboard export

help:                            ## Show this help
	@awk 'BEGIN {FS = ":.*?## "} /^[a-zA-Z_-]+:.*?## / { printf "  \033[36m%-12s\033[0m %s\n", $$1, $$2 }' $(MAKEFILE_LIST)

install:                         ## Install the kit (Brewfile + stow) and fleetc onto PATH
	./install.sh
	cargo install --path crates/fleetc --force

uninstall:                       ## Uninstall the kit and remove fleetc
	-cargo uninstall fleetc
	./install.sh --uninstall

# The fleet targets need P=<name> — the project folder projects/<name>/.
.project:
	@if [ -z "$(P)" ]; then echo "P=<name> required — one of: $$(command ls projects | tr '\n' ' ')"; exit 1; fi

preflight: .project              ## Check project P's setup: runtime state, checkout, forge + tracker CLIs, tmux + claude
	cargo run -q -p fleetd -- -C projects/$(P) preflight

# One fleet at a time: `run` refuses while another project's fleetd is up. fleetd itself refuses
# when its own socket already answers, so a second `run` just reopens the dashboard. fleetd logs
# to projects/P/.fleet/fleetd.log (the dashboard's Log tab).
run: .project                    ## Start project P's fleetd in the background (unless it's up), then open its dashboard
	@pids=$$(pgrep -x fleetd | paste -sd, -); if [ -n "$$pids" ] && ps -o args= -p "$$pids" | grep -v '/fleetd -C projects/$(P)$$'; then echo "↑ another project's fleetd is up — make stop P=<it> first"; exit 1; fi
	@cargo build -q -p fleetd
	@nohup target/debug/fleetd -C projects/$(P) </dev/null >/dev/null 2>&1 &
	@$(MAKE) --no-print-directory dashboard P=$(P)

export: .project                 ## Snapshot project P's ledger to projects/P/exports/P-<UTC stamp>.db (safe while running)
	cargo run -q -p fleetc -- -C projects/$(P) export "projects/$(P)/exports/$(P)-$$(date -u +%Y%m%dT%H%M%SZ).db"

stop: .project                   ## Stop project P's fleet daemon
	@pkill -f '/fleetd -C projects/$(P)$$' && echo "fleetd stopped ($(P))" || echo "fleetd not running ($(P))"

dashboard: .project              ## Build + run the TUI dashboard over project P (cargo run --release)
	cargo run --release -p dashboard -- -C projects/$(P)
