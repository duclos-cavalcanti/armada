# Armada — operator entry points.
#
# The kit is the fleet: one self-ticking daemon (fleetd) + its client (fleetc).
# `install.sh` gates the Brewfile, stows agents/skills (+ the .claude config) and installs fleetc;
# `fleet.sh` builds + runs a project's fleet. A project is a folder, projects/<name>/ (its
# fleet.toml + .fleet/ state + checkout): every fleet target takes FLEET_PROJECT=<name> (only help / install / uninstall / serve-* don't). Run from
# the repo: `make <target> FLEET_PROJECT=<name>` — or `export FLEET_PROJECT=<name>` once per shell.
#
# `make` with no target prints the help summary.

.DEFAULT_GOAL := help

.PHONY: help install uninstall serve-fleet serve-slides serve-notes preflight run restart stop dashboard snapshot import

help:                            ## Show this help
	@awk 'BEGIN {FS = ":.*?## "} /^[a-zA-Z_-]+:.*?## / { printf "  \033[36m%-12s\033[0m %s\n", $$1, $$2 }' $(MAKEFILE_LIST)

install:                         ## Install the kit (Brewfile + stow) and fleetc onto PATH
	./install.sh

uninstall:                       ## Uninstall the kit and remove fleetc
	./install.sh --uninstall

demos/node_modules demos/fleet/node_modules:
	npm ci --prefix $(@D)

serve-fleet: demos/fleet/node_modules  ## Serve the fleet viewer on :5173, until Ctrl-C
	npm --prefix demos/fleet run dev -- --port 5173

serve-slides: demos/node_modules       ## Serve the slides on :5174, until Ctrl-C
	npm --prefix demos run dev -- --port 5174 --no-skills-check

# The repo's markdown as a wiki in the browser (Madness, in Docker; settings in .madness.yml),
# mounted read-only. Madness's search walks every folder on each query (its exclude only trims
# the sidebar), so the heavy ones are hidden from it behind empty tmpfs mounts.
NOTES_HIDE := $(wildcard projects target third-party apps/*/target \
	demos/node_modules demos/*/node_modules demos/dist demos/*/dist)

serve-notes:                     ## Serve the repo's markdown as a wiki on :5175, until Ctrl-C
	@echo "serve-notes: http://localhost:5175"
	docker run --rm -it --name armada-notes \
		-p 127.0.0.1:5175:3000 \
		-v "$(CURDIR)":/docs:ro \
		$(foreach d,$(NOTES_HIDE),--tmpfs /docs/$(d)) \
		dannyben/madness:1.3.1 server

preflight:                       ## Check the project's setup: runtime state, checkout, forge + tracker CLIs, tmux + claude
	@./fleet.sh preflight

run:                             ## Restart fleetd (see restart), then open its dashboard
	@./fleet.sh run

restart:                         ## Build fleetd + reinstall fleetc, (re)start fleetd in the background
	@./fleet.sh restart

stop:                            ## Stop the project's fleet daemon (waits up to 5s for it to exit)
	@./fleet.sh stop

dashboard:                       ## Build + run the TUI dashboard over the project (cargo run --release)
	@./fleet.sh dashboard

snapshot:                        ## Snapshot the project's ledger to projects/<name>/exports/<name>-<UTC stamp>.db (safe while running)
	@./fleet.sh snapshot

import:                          ## Restore a ledger snapshot: FILE=<snapshot> (the fleet must be stopped)
	@./fleet.sh import "$(FILE)"
