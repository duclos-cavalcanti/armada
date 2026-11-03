#!/usr/bin/env bash
#
# admiralty.sh — spawn an admiral to rule on a ticket's confirmed escalation, and open it
# in a floating tmux popup. The admiral grounds on the canon, then reads the verdict itself. Its
# session (admiral-<KEY>) outlives the popup: run this again to attach to the same conversation.
# --no-floating spawns it in the background only, so several can be queued while you work.
#
# Usage: scripts/admiralty.sh [--no-floating] <project-dir> <KEY>
set -euo pipefail

die() { echo "$*" >&2; exit 1; }

# Attach to the session — in a popup when inside tmux. The target is single-quoted: the popup runs
# it in the operator's shell, and zsh expands a leading `=`.
popup() {
    if [ -n "${TMUX:-}" ]; then
        tmux display-popup -E -w 90% -h 85% "TMUX= tmux attach -t '=$1'"
    else
        tmux attach -t "=$1"
    fi
}

# Refuse unless the ticket's latest verdict is CONFIRM — a REFUTE already went back to the cog.
# Checked on every run: an admiral left over from an older escalation is no longer the one to ask.
confirmed() {
    local project="$1" key="$2" verdict
    verdict="$(fleetc -C "$project" inspection "$key")" || exit 1
    verdict="${verdict%%$'\n'*}"
    case "$verdict" in
        CONFIRM*) ;;
        *) die "$key's escalation isn't confirmed ($verdict): nothing to rule on" ;;
    esac
}

start() {
    local project="$1" key="$2" session="$3" repo ask
    repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
    ask="Escalation on $key, confirmed by inspection. Read its verdict (fleetc inspection $key), then help me rule on it."
    # --add-dir first: it takes a list, and would swallow the message as another directory.
    tmux new-session -d -s "$session" -c "$project" \
        "$(printf '%q ' claude --add-dir "$repo" --agent admiral "$ask")"
}

main() {
    local usage="usage: admiralty.sh [--no-floating] <project-dir> <KEY>"
    local floating=1 project key session
    if [ "${1:-}" = "--no-floating" ]; then floating=0; shift; fi
    project="${1:?$usage}" key="${2:?$usage}"
    project="$(cd "$project" && pwd)" || die "no project folder $1"
    session="admiral-$key"
    confirmed "$project" "$key"
    tmux has-session -t "=$session" 2>/dev/null || start "$project" "$key" "$session"
    if [ "$floating" = 1 ]; then popup "$session"; else echo "$session"; fi
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
    main "$@"
fi
