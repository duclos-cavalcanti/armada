#!/usr/bin/env bash
# report.sh — render a convoy's progress report: one self-contained HTML file.
#   usage: report.sh <project-dir> <convoy KEY|#N> [<out.html>]
# Reads only: `fleetc context` + the project's ledger, read-only. Prints the report's path, then
# its summary (counts, grade, the critical path's) as JSON.
set -euo pipefail

die() { echo "report.sh: $*" >&2; exit 1; }

main() {
  [[ $# -ge 2 ]] || die "usage: report.sh <project-dir> <convoy KEY|#N> [<out.html>]"
  local proj=$1 convoy=$2 out=${3:-}
  local here ledger ctx key first led url now data
  here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
  ledger="$proj/.fleet/ledger.db"
  [[ -f $ledger ]] || die "no ledger at $ledger (is $proj a fleet project?)"

  ctx=$(fleetc -C "$proj" context --convoy "$convoy")
  key=$(jq -r .convoy <<<"$ctx")
  led=$(sqlite3 -readonly "$ledger" ".parameter set :convoy \"'$key'\"" ".read $here/ledger.sql")
  # The tracker's link for one ticket, its key made a placeholder; none when the tracker gives none.
  first=$(jq -r '.tickets[0].key // empty' <<<"$ctx")
  url=$( { [[ -n $first ]] && fleetc -C "$proj" ticket "$first" --where 2>/dev/null; } \
         | sed "s/$first/{key}/" || true)
  now=$(date -u +%Y-%m-%dT%H:%M:%SZ)

  data=$(jq -n --argjson ctx "$ctx" --argjson led "$led" --arg url "$url" --arg now "$now" \
           --arg project "$(basename "$(cd "$proj" && pwd)")" -f "$here/summary.jq")

  if [[ -z $out ]]; then
    mkdir -p "$proj/exports"
    # <convoy title>_<UTC date>.html, the title made a file name: lowercase, hyphenated.
    out="$proj/exports/$(jq -r '.convoy.title // .convoy.key' <<<"$data" | tr '[:upper:]' '[:lower:]' \
          | sed -E 's/[^a-z0-9]+/-/g; s/^-+|-+$//g')_$(date -u +%Y-%m-%d).html"
  fi
  # `</` escaped so no ticket text can close the data's <script>; ENVIRON, as `awk -v` unescapes.
  DATA=${data//<\//<\\/} awk '$0 == "__DATA__" { print ENVIRON["DATA"]; next } { print }' \
    "$here/../template.html" > "$out"

  echo "$out"
  jq '{convoy: .convoy | {key, no, title}, total, cancelled, grade, counts, critical: (.critical | del(.named))}' <<<"$data"
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then main "$@"; fi
