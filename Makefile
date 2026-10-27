# Armada — operator entry points.
#
# The kit is the fleet: one self-ticking daemon (fleetd) + its client (fleetc).
# `install.sh` gates the Brewfile and stows agents/skills (+ the .claude config); `cargo` builds
# the binaries. A project is a folder, projects/<name>/ (its fleet.toml + .fleet/ state + checkout):
# every fleet target takes FLEET_PROJECT=<name> (only help / install / uninstall don't). Run from
# the repo: `make <target> FLEET_PROJECT=<name>` — or `export FLEET_PROJECT=<name>` once per shell.
#
# `make` with no target prints the help summary.

.DEFAULT_GOAL := help
.PHONY: help install uninstall .project preflight run stop dashboard admiral export

help:                            ## Show this help
	@awk 'BEGIN {FS = ":.*?## "} /^[a-zA-Z_-]+:.*?## / { printf "  \033[36m%-12s\033[0m %s\n", $$1, $$2 }' $(MAKEFILE_LIST)

install:                         ## Install the kit (Brewfile + stow) and fleetc onto PATH
	./install.sh
	cargo install --path crates/fleetc --force

uninstall:                       ## Uninstall the kit and remove fleetc
	-cargo uninstall fleetc
	./install.sh --uninstall

.project:
	@if [ -z "$(FLEET_PROJECT)" ]; then echo "FLEET_PROJECT=<name> required (or export it) — one of: $$(command ls projects | tr '\n' ' ')"; exit 1; fi

preflight: .project              ## Check the project's setup: runtime state, checkout, forge + tracker CLIs, tmux + claude
	cargo run -q -p fleetd -- -C projects/$(FLEET_PROJECT) preflight

run: .project                    ## Start the project's fleetd in the background (unless it's up), then open its dashboard
	@pids=$$(pgrep -x fleetd | paste -sd, -); if [ -n "$$pids" ] && ps -o args= -p "$$pids" | grep -v '/fleetd -C projects/$(FLEET_PROJECT)$$'; then echo "↑ another project's fleetd is up — make stop FLEET_PROJECT=<it> first"; exit 1; fi
	@cargo build -q -p fleetd
	@nohup target/debug/fleetd -C projects/$(FLEET_PROJECT) </dev/null >/dev/null 2>&1 &
	@$(MAKE) --no-print-directory dashboard

export: .project                 ## Snapshot the project's ledger to projects/<name>/exports/<name>-<UTC stamp>.db (safe while running)
	cargo run -q -p fleetc -- -C projects/$(FLEET_PROJECT) export "projects/$(FLEET_PROJECT)/exports/$(FLEET_PROJECT)-$$(date -u +%Y%m%dT%H%M%SZ).db"

stop: .project                   ## Stop the project's fleet daemon
	@pkill -f '/fleetd -C projects/$(FLEET_PROJECT)$$' && echo "fleetd stopped ($(FLEET_PROJECT))" || echo "fleetd not running ($(FLEET_PROJECT))"

dashboard: .project              ## Build + run the TUI dashboard over project P (cargo run --release)
	cargo run --release -p dashboard -- -C projects/$(FLEET_PROJECT)

admiral: .project                ## Talk to the project's admiral (its product expert; reads the kit, never edits it)
	cd projects/$(FLEET_PROJECT) && claude --agent admiral --add-dir ../..
