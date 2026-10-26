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
#   fleet/dispatched   $1=ticket  $2=crew(worker|hand)
#   fleet/impl         $1=ticket  $2=kind  $3=tmux_session  $4=state  $5=mr_iid
#   fleet/merge        $1=ticket  $2=mr_iid  $3=url  $4=sailor_tmux(only when up)  $5=sailor_state
#   fleet/escalations  $1=ticket  $2=raised_by  $3=ship_iid
#   ships/list         $1=iid  $2=ticket  $3=pilot  $4=url
#   board/tickets      $1=key  $2=status  $3=convoy
#   board/convoys      $1=key  $2=title  $3=captain_tmux(live only)
#
# Forge- and tracker-neutral: links come from the fleet itself (`fleetc -C "$FLEET_PROJECT"`,
# the project folder the dashboard passes in), never from hardcoded hosts.
set -euo pipefail

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

launch_browser() { open "$1"; }
fleet()          { fleetc -C "${FLEET_PROJECT:?the dashboard sets FLEET_PROJECT}" "$@"; }
# A ticket opens where its tracker keeps it: a URL (Jira) or a file (ticket files).
launch_ticket()  { local where; where="$(fleet ticket "$1" --where)" && open "$where"; }
# An MR opens at the URL the forge reported for it.
launch_mr()      { local url; url="$(fleet ship --iid "$1" | jq -r '.ship.url')" && launch_browser "$url"; }

# main — map a (coordinate, key) + the focused row's fields to an action.
main() {
    local coord="${1:?usage: handler.sh <tab/pane> <key> [row-fields...]}"
    local key="${2:?usage: handler.sh <tab/pane> <key> [row-fields...]}"
    shift 2   # remaining positionals ($1..) are the focused row's fields

    case "$coord/$key" in
        # ---- Fleet · dispatched / impl / merge (the line, by phase) --------
        # t opens the ticket everywhere; enter attaches the cog at work; m opens the MR.
        fleet/dispatched/enter | fleet/dispatched/t) launch_ticket "${1:-}" ;;
        fleet/impl/enter) launch_tmux "${3:-}" ;;
        fleet/impl/t) launch_ticket "${1:-}" ;;
        fleet/impl/m) [ -n "${5:-}" ] && launch_mr "${5}" || echo "${1:-} has no MR yet" >&2 ;;
        # enter attaches the live sailor, or — none at work — opens the MR.
        fleet/merge/enter)
            if [ -n "${4:-}" ]; then launch_tmux "${4}"; else launch_browser "${3:-}"; fi
            ;;
        fleet/merge/t) launch_ticket "${1:-}" ;;
        fleet/merge/m) launch_browser "${3:-}" ;;

        # ---- Fleet · escalations ------------------------------------------
        fleet/escalations/t) launch_ticket "${1:-}" ;;
        fleet/escalations/m)
            [ -n "${3:-}" ] || { echo "no MR associated with ${1:-}" >&2; exit 1; }
            launch_mr "${3:-}"
            ;;

        # ---- Ships --------------------------------------------------------
        # enter opens the MR — by its url ($4) if present, else by iid ($1); l attaches the lookout.
        ships/list/enter)
            if [ -n "${4:-}" ]; then launch_browser "${4}"; else launch_mr "${1:-}"; fi
            ;;
        ships/list/t) [ -n "${2:-}" ] && launch_ticket "${2:-}" || echo "allied ship has no ticket" >&2 ;;
        ships/list/l) launch_tmux lookout ;;

        # ---- Board · tickets / convoys ------------------------------------
        board/tickets/enter | board/tickets/t) launch_ticket "${1:-}" ;;
        board/convoys/enter) launch_ticket "${1:-}" ;;
        board/convoys/a) [ -n "${3:-}" ] && launch_tmux "${3}" || echo "no live captain on ${1:-}" >&2 ;;

        *) : ;; # unhandled (coordinate, key) — no-op
    esac
}

if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
    main "$@"
fi
