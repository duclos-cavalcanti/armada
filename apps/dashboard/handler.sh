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
# An unhandled (coordinate, key) pair is a silent no-op. A failure exits non-zero with one line
# on stderr — the dashboard shows it, in red, in its footer.
#
# The keys are fixed, never "smart": enter switches to the row's tmux session (or fails),
# m opens the MR, t opens the ticket.
#
# Row-field contract per coordinate (coord + key are stripped, so $1 is the first
# field — these positions match the dashboard's `focused_action()`):
#   fleet/dispatched   $1=ticket  $2=crew(worker|hand)
#   fleet/impl         $1=ticket  $2=kind  $3=tmux_session  $4=state  $5=mr_iid
#   fleet/merge        $1=ticket  $2=mr_iid  $3=url  $4=sailor_tmux  $5=sailor_state
#   ships/list         $1=iid  $2=ticket  $3=pilot  $4=url
#   board/tickets      $1=key  $2=status  $3=convoy
#   convoys            $1=key  $2=title  $3=captain_tmux     (Fleet + Board)
#   proposals          $1=kind(proposal|anchored)  $2=ref(proposal id | ticket key)   (Fleet + Board)
#
# Forge- and tracker-neutral: links come from the fleet itself (`fleetc -C "$FLEET_PROJECT_DIR"`,
# the project folder the dashboard passes in), never from hardcoded hosts.
set -euo pipefail

# Fail with one line on stderr — the dashboard shows it in red.
fail() { echo "$*" >&2; return 1; }

# Switch the current tmux client to a cog's session: `launch_tmux <session> <what's missing>`.
# Fails if the row has no session, or it isn't alive.
launch_tmux() {
    [ -n "$1" ] || fail "$2"
    tmux has-session -t "=$1" 2>/dev/null || fail "no tmux session $1 (dead or not yet spawned)"
    tmux switch-client -t "=$1"
}

launch_browser() { open "$1"; }
fleet()          { fleetc -C "${FLEET_PROJECT_DIR:?the dashboard sets FLEET_PROJECT_DIR}" "$@"; }
# A ticket opens where its tracker keeps it: a URL (Jira) or a file (ticket files).
launch_ticket()  { local where; where="$(fleet ticket "$1" --where)" && open "$where"; }
# An MR opens at the URL the forge reported for it.
launch_mr()      { local url; url="$(fleet ship --iid "$1" | jq -r '.ship.url')" && launch_browser "$url"; }
# The fleetc command line for `fleet "$@"`, shell-quoted — for tmux to run.
fleet_cmd()      { printf '%q ' fleetc -C "${FLEET_PROJECT_DIR:?}" "$@"; }
# Read a proposal in full, in a tmux popup over the dashboard.
read_proposal()  { tmux display-popup -E -w 90% -h 80% "$(fleet_cmd proposal "$1") | less"; }
# Run `fleet "$@"` once the operator answers y in the tmux status line: `confirm_fleet <prompt> <args>…`.
confirm_fleet()  { local prompt="$1"; shift; tmux confirm-before -p "$prompt (y/n)" "run-shell -b '$(fleet_cmd "$@")'"; }
# Accept (a new ticket is filed into the tracker) or drop a proposal.
settle_proposal() { confirm_fleet "$1 proposal $2?" proposal "$1" "$2"; }

# main — map a (coordinate, key) + the focused row's fields to an action.
main() {
    local coord="${1:?usage: handler.sh <tab/pane> <key> [row-fields...]}"
    local key="${2:?usage: handler.sh <tab/pane> <key> [row-fields...]}"
    shift 2   # remaining positionals ($1..) are the focused row's fields

    case "$coord/$key" in
        # ---- Convoys (Fleet + Board) -------------------------------------
        convoys/enter) launch_tmux "${3:-}" "convoy ${1:-} has no captain" ;;
        convoys/t) launch_ticket "${1:-}" ;;

        # ---- Proposals (Fleet + Board): proposals + anchored convoy tickets ----
        # enter reads a proposal; a/d settle it; t opens an anchored ticket, w weighs it.
        proposals/enter) [ "${1:-}" = proposal ] || fail "an anchored ticket: t opens it, w weighs it"; read_proposal "${2:-}" ;;
        proposals/t) [ "${1:-}" = anchored ] || fail "a proposal isn't a ticket yet: enter reads it"; launch_ticket "${2:-}" ;;
        proposals/a) [ "${1:-}" = proposal ] || fail "only a proposal is accepted"; settle_proposal accept "${2:-}" ;;
        proposals/d) [ "${1:-}" = proposal ] || fail "only a proposal is dropped"; settle_proposal drop "${2:-}" ;;
        proposals/w) [ "${1:-}" = anchored ] || fail "only an anchored ticket is weighed"; confirm_fleet "weigh ${2:-}?" weigh "${2:-}" ;;

        # ---- Fleet · dispatched / impl / merge (the line, by phase) --------
        fleet/dispatched/t) launch_ticket "${1:-}" ;;
        fleet/impl/enter) launch_tmux "${3:-}" "${1:-} has no ${2:-cog} session" ;;
        fleet/impl/t) launch_ticket "${1:-}" ;;
        fleet/impl/m) [ -n "${5:-}" ] || fail "${1:-} has no MR yet"; launch_mr "${5}" ;;
        fleet/merge/enter) launch_tmux "${4:-}" "${1:-} has no sailor" ;;
        fleet/merge/t) launch_ticket "${1:-}" ;;
        fleet/merge/m) launch_browser "${3:-}" ;;

        # ---- Ships --------------------------------------------------------
        # m opens the MR — by its url ($4) if present, else by iid ($1); l attaches the lookout.
        ships/list/m) if [ -n "${4:-}" ]; then launch_browser "${4}"; else launch_mr "${1:-}"; fi ;;
        ships/list/t) [ -n "${2:-}" ] || fail "an allied ship has no ticket"; launch_ticket "${2:-}" ;;
        ships/list/l) launch_tmux lookout "no lookout" ;;

        # ---- Board · tickets -----------------------------------------------
        board/tickets/t) launch_ticket "${1:-}" ;;

        # enter / m on a row with no session / MR.
        */enter) fail "no tmux session on this row — t ticket · m MR" ;;
        */m) fail "no MR on this row" ;;
        *) : ;; # unhandled (coordinate, key) — no-op
    esac
}

if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
    main "$@"
fi
