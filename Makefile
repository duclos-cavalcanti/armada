# Armada — operator entry points.

.DEFAULT_GOAL := help

.PHONY: help install uninstall serve slides viewer preflight run restart stop dashboard snapshot import

help:                            ## Show this help
	@awk 'BEGIN {FS = ":.*?## "} /^[a-zA-Z_-]+:.*?## / { printf "  \033[36m%-12s\033[0m %s\n", $$1, $$2 }' $(MAKEFILE_LIST)

install:                         ## Install the kit (Brewfile + stow) and fleetc onto PATH
	./install.sh

uninstall:                       ## Uninstall the kit and remove fleetc
	./install.sh --uninstall

demos/node_modules site/node_modules:
	npm ci --prefix $(@D)

DEMO        ?= fleet
SLIDES_PORT ?= 5174
VIEWER_PORT ?= 5177

slides: demos/node_modules             ## Serve a demos project's slides: DEMO=<project> (default fleet), on :5174 (SLIDES_PORT), until Ctrl-C
	@test -d demos/$(DEMO)/slides || { echo "DEMO=$(DEMO): no demos/$(DEMO)/slides/" >&2; exit 1; }
	DEMO=$(DEMO) npm --prefix demos run dev -- --port $(SLIDES_PORT) --no-skills-check

viewer: demos/node_modules             ## Serve a demos project's architecture viewer alone: DEMO=<project> (default fleet), on :5177 (VIEWER_PORT), until Ctrl-C
	@test -d demos/$(DEMO)/viewer || { echo "DEMO=$(DEMO): no demos/$(DEMO)/viewer/" >&2; exit 1; }
	cd demos && node --input-type=module -e "const { createServer } = await import('vite'); \
	  const server = await createServer({ configFile: false, root: '$(DEMO)/viewer', cacheDir: '$(CURDIR)/demos/node_modules/.vite-viewer-$(DEMO)', server: { port: $(VIEWER_PORT), strictPort: true } }); \
	  await server.listen(); server.printUrls();"

serve: site/node_modules         ## Serve the notes (markdown, fleet viewer, reports/) on :5175, until Ctrl-C
	NODE_OPTIONS=--max-old-space-size=8192 npm --prefix site start -- --port 5175

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
