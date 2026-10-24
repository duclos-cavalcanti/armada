#!/usr/bin/env bash
# apply.sh — rebuild .claude/settings.json as the committed base + one harness profile.
#
#   harness/apply.sh harness/profiles/<name>.json
#
# A profile is a settings fragment (autoMode, permissions, …) for one deployment —
# work, personal. Objects merge deeply; arrays concatenate without duplicates. The
# base is always HEAD's settings.json, so swapping profiles drops the old one cleanly
# (uncommitted edits to settings.json are discarded). The result is what `make install`
# stows into ~/.claude — never commit it while a profile is applied. Revert with
# `git checkout .claude/settings.json`.
set -euo pipefail

# shellcheck disable=SC2016 # jq program, not shell expansion
MERGE='
def merge($a; $b):
  if ($a | type) == "object" and ($b | type) == "object" then
    reduce ($b | keys_unsorted[]) as $k ($a; .[$k] = merge($a[$k]; $b[$k]))
  elif ($a | type) == "array" and ($b | type) == "array" then
    reduce ($a + $b)[] as $x ([]; if any(.[]; . == $x) then . else . + [$x] end)
  elif $b == null then $a
  else $b end;
merge(.[0]; .[1])'

main() {
    local profile="${1:?usage: harness/apply.sh harness/profiles/<name>.json}"
    [[ -f "$profile" ]] || { echo "error: no profile at $profile" >&2; exit 1; }
    local repo merged
    repo="$(git -C "$(dirname "$0")" rev-parse --show-toplevel)"
    merged="$(jq -s "$MERGE" <(git -C "$repo" show HEAD:.claude/settings.json) "$profile")"
    printf '%s\n' "$merged" > "$repo/.claude/settings.json"
    echo "applied $profile → .claude/settings.json (don't commit it while applied)"
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
    main "$@"
fi
