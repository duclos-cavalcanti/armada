#!/usr/bin/env bash
#
# install.sh — stow this kit into a harness provider's config tree (or tear it down).
#
# The repo ROOT is the source of truth: agents/ and skills/ live at the top level,
# harness-neutral, and stow into whichever provider you name (claude default; pi,
# gpt, … → ~/.<harness>). For the CLAUDE harness only, it additionally stows the
# loose Claude config (.claude/*) into ~/.claude. The fleet's state isn't installed: each
# project keeps its own, in projects/<name>/.fleet/ (fleetd creates it on first run).
#
# Usage:
#   ./install.sh                     install for the default harness (claude)
#   ./install.sh -H|--harness pi     install agents/ + skills/ for a named harness
#   ./install.sh --home <dir>        override the harness home dir
#   ./install.sh -u|--uninstall      uninstall (unstow) — the mirror of install
#   ./install.sh -h|--help           this help
#
# LAYOUT (repo root = source of truth):
#   agents/            → <harness-home>/agents/        (per-file, --no-folding; the
#                        dir is shared with the operator's own agents)
#   skills/<name>/     → <harness-home>/skills/<name>  (dir-as-unit fold, so
#                        post-install edits propagate through the symlink)
#   CLAUDE HARNESS ONLY, additionally:
#   .claude/*          → ~/.claude/   (loose config: CLAUDE.md, settings.json,
#                        keybindings.json, scripts/; agent-memory/ + settings.local.json
#                        are never stowed — project-/machine-scoped)
#
# HARNESS RESOLUTION — <harness-home> defaults to ~/.<harness> (claude → ~/.claude).
# Override per-run with --home, or (claude) via $CLAUDE_HOME.
set -euo pipefail

# ---- Globals (set in main / resolve_harness) ----
# CLAUDE_HOME is read from the environment with a $HOME fallback inside resolve_harness
# — deliberately NOT pre-declared, so a `FOO=""` line can't clobber an override.
REPO=""
HARNESS="claude"
HOME_OVERRIDE=""
HARNESS_HOME=""
AGENTS_TARGET=""
SKILLS_TARGET=""
CLAUDE_TARGET=""

# ---- Prerequisite gate ----
check_prereqs() {
    if ! command -v stow >/dev/null; then
        echo "error: GNU Stow is required." >&2
        echo "  install: brew install stow" >&2
        exit 1
    fi
    if [[ ! -d "$REPO/agents" || ! -d "$REPO/skills" ]]; then
        echo "error: expected agents/ and skills/ at repo root ($REPO)." >&2
        exit 1
    fi
    # The full Brewfile gate (the fleet's runtime prereqs) applies only to the claude
    # install — another harness only needs stow + the agents/skills dirs above.
    [[ "$HARNESS" == "claude" ]] || return 0
    if ! command -v brew >/dev/null; then
        echo "error: Homebrew is required for the claude/fleet install." >&2
        echo "  install: https://brew.sh/" >&2
        exit 1
    fi
    local brewfile="$REPO/Brewfile"
    if [[ ! -f "$brewfile" ]]; then
        echo "error: $brewfile not found — cannot verify prerequisites." >&2
        exit 1
    fi
    if ! brew bundle check --no-upgrade --file="$brewfile" >/dev/null 2>&1; then
        echo "error: Brewfile entries not installed via brew:" >&2
        brew bundle check --no-upgrade --verbose --file="$brewfile" 2>&1 | sed 's/^/  /' >&2
        echo >&2
        echo "  install: brew bundle --file=\"$brewfile\"" >&2
        exit 1
    fi
}

# ---- Harness resolution ----
# Maps a harness name to its config home. Extend the case for any provider whose
# layout diverges from the ~/.<harness>/{agents,skills} default.
resolve_harness() {
    case "$HARNESS" in
        claude) HARNESS_HOME="${CLAUDE_HOME:-$HOME/.claude}" ;;
        *)      HARNESS_HOME="$HOME/.$HARNESS" ;;
    esac
    [[ -n "$HOME_OVERRIDE" ]] && HARNESS_HOME="$HOME_OVERRIDE"
    mkdir -p "$HARNESS_HOME/agents" "$HARNESS_HOME/skills"
    # Canonicalize so stow computes correct relative links (resolves symlinked path
    # components like $HOME and macOS /var→/private/var).
    HARNESS_HOME="$(cd "$HARNESS_HOME" && pwd -P)"
    AGENTS_TARGET="$HARNESS_HOME/agents"
    SKILLS_TARGET="$HARNESS_HOME/skills"
    CLAUDE_TARGET="$HARNESS_HOME" # only used by the claude-only phase
}

# ---- agents/ + skills/ (harness-agnostic) ----
# --target points at the FINAL destination dir (stow strips the package name).
stow_agents() {
    stow --dir "$REPO" --target "$AGENTS_TARGET" \
        --restow --no-folding --verbose --ignore='^\.stow-manifest' agents
}
stow_skills() {
    stow --dir "$REPO" --target "$SKILLS_TARGET" \
        --restow --verbose --ignore='^\.stow-manifest' skills
}
unstow_agents() {
    stow --dir "$REPO" --target "$AGENTS_TARGET" \
        --delete --no-folding --verbose --ignore='^\.stow-manifest' agents
}
unstow_skills() {
    stow --dir "$REPO" --target "$SKILLS_TARGET" \
        --delete --verbose --ignore='^\.stow-manifest' skills
}

# ---- claude-only: the loose .claude/ root config → ~/.claude ----
# agents/skills live top-level now (own phases above); agent-memory/ (project-scoped)
# + settings.local.json (per-machine) never leave the repo.
stow_claude_global() {
    stow --dir "$REPO" --target "$CLAUDE_TARGET" \
        --restow --no-folding --verbose \
        --ignore='^agent-memory$' --ignore='^settings\.local\.json$' --ignore='^\.stow-manifest' \
        .claude
}
unstow_claude_global() {
    stow --dir "$REPO" --target "$CLAUDE_TARGET" \
        --delete --no-folding --verbose \
        --ignore='^agent-memory$' --ignore='^settings\.local\.json$' --ignore='^\.stow-manifest' \
        .claude
}

# ---- Direction drivers ----
install() {
    check_prereqs
    resolve_harness
    echo "installing agents/ + skills/ → $HARNESS_HOME (harness: $HARNESS)"
    stow_agents
    stow_skills
    if [[ "$HARNESS" == "claude" ]]; then
        stow_claude_global
    fi
}

uninstall() {
    resolve_harness
    echo "uninstalling agents/ + skills/ from $HARNESS_HOME (harness: $HARNESS)"
    if [[ "$HARNESS" == "claude" ]]; then
        unstow_claude_global
    fi
    unstow_skills
    unstow_agents
}

usage() { sed -n '11,16p' "${BASH_SOURCE[0]}" | sed 's/^#[[:space:]]*//;s/^#//'; }

main() {
    local mode=install
    while [[ $# -gt 0 ]]; do
        case "$1" in
            install)            mode=install ;;
            -u|--uninstall)     mode=uninstall ;;
            -H|--harness)       HARNESS="${2:?--harness needs a value}"; shift ;;
            --home)             HOME_OVERRIDE="${2:?--home needs a value}"; shift ;;
            -h|--help)          usage; exit 0 ;;
            *) echo "install.sh: unknown argument '$1'" >&2; usage >&2; exit 2 ;;
        esac
        shift
    done

    REPO="$(git -C "$(dirname "${BASH_SOURCE[0]}")" rev-parse --show-toplevel)"
    REPO="$(cd "$REPO" && pwd -P)"

    if [[ "$mode" == "uninstall" ]]; then uninstall; else install; fi
}

main "$@"
