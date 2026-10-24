#!/bin/bash
#
# Claude Code hook router — single entry for all hooks. Pure passthrough: forward
# the hook event (and any args) to the crew backend if installed; no-op otherwise.
# No logic here — crew owns the event->state mapping. Always exits 0.
#
# Wired from .claude/settings.json:  bash ~/.claude/scripts/signal.sh <EVENT> [args...]

crew="$HOME/.config/crew/crew"
[ -x "$crew" ] || exit 0

"$crew" signal "$@"
exit 0
