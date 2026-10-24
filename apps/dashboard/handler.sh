#!/usr/bin/env bash
# handler.sh — the dashboard's single action handler.
#
# The dashboard TUI is a generic engine: it reads the ledger, renders
# tabs → panes and, on a non-navigation keypress over a focused row, invokes:
#
#   handler.sh <tab/pane> <key> <row-field>...
#
# This script owns the ENTIRE (coordinate, key) → action mapping. Adding or
# changing an action is a one-arm edit here; the dashboard binary never changes.
# An unhandled (coordinate, key) pair is a silent no-op.
#
# Row-field contract per coordinate (coord + key are stripped, so $1 is the first
# field — these positions match the dashboard's `focused_action()`):
#   fleet/cogs         $1=kind  $2=ticket  $3=convoy  $4=tmux_session  $5=state
#   fleet/escalations  $1=ticket  $2=raised_by  $3=ship_iid
#   armada/ships       $1=iid  $2=ticket  $3=pilot  $4=url
#   board/tickets      $1=key  $2=status  $3=convoy
#   board/convoys      $1=key  $2=title
set -euo pipefail

readonly GITLAB_MR_BASE="<redacted>"
readonly JIRA_BROWSE_BASE="<redacted>"

# Switch the current tmux client to a cog's session (requires running inside tmux),
# or report to stderr and fail if the session isn't alive.
launch_tmux() {
    local session="$1"
    if ! tmux has-session -t "=$session" 2>/dev/null; then
        echo "no $session tmux session (dead or not yet spawned)" >&2
        return 1
    fi
    tmux switch-client -t "=$session"
}

launch_browser()     { open "$1"; }
launch_gitlab_mr()   { launch_browser "$GITLAB_MR_BASE/$1"; }
launch_jira_ticket() { launch_browser "$JIRA_BROWSE_BASE/$1"; }

# main — map a (coordinate, key) + the focused row's fields to an action.
main() {
    local coord="${1:?usage: handler.sh <tab/pane> <key> [row-fields...]}"
    local key="${2:?usage: handler.sh <tab/pane> <key> [row-fields...]}"
    shift 2   # remaining positionals ($1..) are the focused row's fields

    case "$coord/$key" in
        # ---- Fleet · cogs -------------------------------------------------
        # enter attaches the cog's tmux session ($4); t opens its ticket ($2,
        # empty for a ticketless captain/lookout).
        fleet/cogs/enter) launch_tmux "${4:-}" ;;
        fleet/cogs/t) [ -n "${2:-}" ] && launch_jira_ticket "${2:-}" || echo "cog has no ticket" >&2 ;;

        # ---- Fleet · escalations ------------------------------------------
        fleet/escalations/t) launch_jira_ticket "${1:-}" ;;
        fleet/escalations/m)
            [ -n "${3:-}" ] || { echo "no MR associated with ${1:-}" >&2; exit 1; }
            launch_gitlab_mr "${3:-}"
            ;;

        # ---- Armada · ships -----------------------------------------------
        # enter opens the MR — by its url ($4) if present, else by iid ($1).
        armada/ships/enter)
            if [ -n "${4:-}" ]; then launch_browser "${4}"; else launch_gitlab_mr "${1:-}"; fi
            ;;
        armada/ships/t) [ -n "${2:-}" ] && launch_jira_ticket "${2:-}" || echo "peer ship has no ticket" >&2 ;;

        # ---- Board · tickets / convoys ------------------------------------
        board/tickets/enter | board/tickets/t) launch_jira_ticket "${1:-}" ;;
        board/convoys/enter) launch_jira_ticket "${1:-}" ;;

        *) : ;; # unhandled (coordinate, key) — no-op
    esac
}

if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
    main "$@"
fi
