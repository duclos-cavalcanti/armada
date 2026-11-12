#!/usr/bin/env bash
# report.sh — render a convoy's progress report: one self-contained HTML file.
#   usage: report.sh <project-dir> <convoy KEY|#N> [<out.html>]
# Reads only: `fleetc context` + the project's ledger and telemetry, read-only. Prints the report's
# path, then its summary (counts, grade, the critical path's, the spend) as JSON.
set -euo pipefail

die() { echo "report.sh: $*" >&2; exit 1; }

main() {
  [[ $# -ge 2 ]] || die "usage: report.sh <project-dir> <convoy KEY|#N> [<out.html>]"
  local proj=$1 convoy=$2 out=${3:-}
  local here ledger ctx key first led url now spend data name
  here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
  name=$(basename "$(cd "$proj" && pwd)")
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
  # The convoy's spend, as /fleet-cost-report attributes it (its by_convoy rows); [] with no telemetry.
  spend='[]'
  if [[ -f $proj/.fleet/telemetry.db ]]; then
    spend=$(sqlite3 -readonly "$proj/.fleet/telemetry.db" "ATTACH 'file:$ledger?mode=ro' AS l" \
              ".parameter set :since \"''\"" ".read $here/../../fleet-cost-report/scripts/cost.sql" \
            | jq --arg key "$key" '[.by_convoy[] | select(.convoy == $key) | {role, usd}]')
  fi

  data=$(jq -n --argjson ctx "$ctx" --argjson led "$led" --argjson spend "$spend" --arg url "$url" --arg now "$now" \
           --arg project "$name" -f "$here/summary.jq")

  if [[ -z $out ]]; then
    # The repo's reports/, beside projects/: <convoy title>_<UTC date>.html, the title made a file
    # name (lowercase, hyphenated).
    out="$(cd "$proj/../.." && pwd)/reports/$name/convoys"
    mkdir -p "$out"
    out="$out/$(jq -r '.convoy.title // .convoy.key' <<<"$data" | tr '[:upper:]' '[:lower:]' \
          | sed -E 's/[^a-z0-9]+/-/g; s/^-+|-+$//g')_$(date -u +%Y-%m-%d).html"
  fi
  # `</` escaped so no ticket text can close the data's <script>; ENVIRON, as `awk -v` unescapes.
  DATA=${data//<\//<\\/} awk '$0 == "__DATA__" { print ENVIRON["DATA"]; next } { print }' \
    "$here/../template.html" > "$out"

  echo "$out"
  jq '{convoy: .convoy | {key, no, title}, total, cancelled, grade, counts, critical: (.critical | if . then del(.edges, .outside) else . end), escalations, spend: {usd: (.spend.usd * 100 | round / 100)}}' <<<"$data"
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then main "$@"; fi
