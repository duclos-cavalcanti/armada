#!/usr/bin/env bash
# claude_with_excalidraw.sh — a Claude session at the repo root with harness/mcp.json's MCP servers. Their key is
# read from the Keychain into this process only, never your shell.
#
#   harness/claude_with_excalidraw.sh [claude args…]      e.g. harness/claude_with_excalidraw.sh --agent shipwright
set -euo pipefail

main() {
    local repo key
    repo="$(cd "$(dirname "$0")/.." && pwd)"
    key="$(security find-generic-password -a "$USER" -s EXCALIDRAW_API_KEY -w)" || {
        echo "error: no EXCALIDRAW_API_KEY in the Keychain: security add-generic-password -a \"\$USER\" -s EXCALIDRAW_API_KEY -w" >&2
        exit 1
    }
    cd "$repo"
    EXCALIDRAW_API_KEY="$key" exec claude --mcp-config harness/mcp.json "$@"
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
    main "$@"
fi
