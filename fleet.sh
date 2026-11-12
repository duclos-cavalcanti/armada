#!/usr/bin/env bash
#
# fleet.sh — start, stop and watch one project's fleet, and serve the repo's notes. `make` wraps it.
#
# Usage (the project comes from $FLEET_PROJECT; notes takes none):
#   ./fleet.sh restart         build fleetd + reinstall fleetc, (re)start fleetd in the background
#   ./fleet.sh run             restart, then open the dashboard
#   ./fleet.sh stop            stop the project's fleetd (waits up to 5s for it to exit)
#   ./fleet.sh dashboard       build + run the TUI dashboard over the project
#   ./fleet.sh preflight       check the setup: runtime state, checkout, forge + tracker CLIs, tmux + claude
#   ./fleet.sh snapshot        snapshot the ledger to projects/<name>/exports/ (safe while running)
#   ./fleet.sh import <file>   restore a ledger snapshot, path from the repo root (the fleet must be stopped)
#   ./fleet.sh notes           serve the repo's markdown and reports/ as a wiki on :5175, until Ctrl-C
set -euo pipefail

PROJECT="${FLEET_PROJECT:-}"

usage() { sed -n '5,13p' "${BASH_SOURCE[0]}" | sed 's/^#[[:space:]]*//;s/^#//'; }

check() {
    local projects
    projects="$(command ls projects | tr '\n' ' ')"
    if [[ -z "$PROJECT" ]]; then
        echo "FLEET_PROJECT=<name> required (or export it) — one of: $projects" >&2
        exit 1
    fi
    if [[ ! -f "projects/$PROJECT/fleet.toml" ]]; then
        echo "no project '$PROJECT' — one of: $projects" >&2
        exit 1
    fi
}

stop() {
    local match="/fleetd -C projects/$PROJECT\$" _
    if ! pkill -f "$match"; then
        echo "fleetd not running ($PROJECT)"
        return
    fi
    for _ in $(seq 25); do
        if ! pgrep -f "$match" >/dev/null; then
            echo "fleetd stopped ($PROJECT)"
            return
        fi
        sleep 0.2
    done
    echo "fleetd ($PROJECT) still running after 5s" >&2
    return 1
}

restart() {
    # One fleet at a time: a fleetd up on another project blocks this one.
    local pids
    pids="$(pgrep -x fleetd | paste -sd, - || true)"
    if [[ -n "$pids" ]] && ps -o args= -p "$pids" | grep -v "/fleetd -C projects/$PROJECT\$"; then
        echo "↑ another project's fleetd is up — make stop FLEET_PROJECT=<it> first" >&2
        exit 1
    fi
    cargo build -q -p fleetd
    ./install.sh --fleetc
    stop
    nohup target/debug/fleetd -C "projects/$PROJECT" </dev/null >/dev/null 2>&1 &
}

dashboard() { cargo run --release -p dashboard -- -C "projects/$PROJECT"; }

run() {
    restart
    dashboard
}

preflight() { cargo run -q -p fleetd -- -C "projects/$PROJECT" preflight; }

snapshot() {
    local stamp
    stamp="$(date -u +%Y%m%dT%H%M%SZ)"
    cargo run -q -p fleetc -- -C "projects/$PROJECT" export "projects/$PROJECT/exports/$PROJECT-$stamp.db"
}

import() {
    cargo run -q -p fleetc -- -C "projects/$PROJECT" import "${1:?import needs a snapshot: FILE=<snapshot>}"
}

# The repo's markdown and reports/'s pages as a wiki in the browser: Madness, in Docker (settings
# in .madness.yml), the repo mounted read-only. Its search walks every folder on each query (its
# exclude only trims the sidebar), so the heavy ones are hidden from it behind empty tmpfs mounts.
notes() {
    local hide=() d
    for d in projects target third-party apps/*/target \
        demos/node_modules demos/*/node_modules demos/dist demos/*/dist; do
        [[ -e $d ]] && hide+=(--tmpfs "/docs/$d")
    done
    echo "notes: http://localhost:5175"
    docker run --rm -it --name armada-notes -p 127.0.0.1:5175:3000 -v "$PWD":/docs:ro \
        ${hide[@]+"${hide[@]}"} dannyben/madness:1.3.1 server
}

main() {
    cd "$(dirname "${BASH_SOURCE[0]}")" # `stop` matches on the relative paths above
    case "${1:-}" in
        run|restart|stop|dashboard|preflight|snapshot|import) check; "$@" ;;
        notes) notes ;;
        -h|--help) usage ;;
        *) echo "fleet.sh: unknown verb '${1:-}'" >&2; usage >&2; exit 2 ;;
    esac
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
    main "$@"
fi
