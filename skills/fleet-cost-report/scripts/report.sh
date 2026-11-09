#!/usr/bin/env bash
# report.sh — render the fleet's spend as one self-contained HTML dashboard.
#   usage: report.sh <project-dir> [--since <RFC3339|YYYY-MM-DD>] [<out.html>]
# Reads only: the project's telemetry DB and ledger, read-only. Prints the dashboard's path, then
# its headline figures as JSON.
set -euo pipefail

die() { echo "report.sh: $*" >&2; exit 1; }

main() {
  [[ $# -ge 1 ]] || die "usage: report.sh <project-dir> [--since <RFC3339|YYYY-MM-DD>] [<out.html>]"
  local proj=$1 since="" out="" here fleet data
  shift
  while [[ $# -gt 0 ]]; do
    case $1 in
      --since) [[ $# -ge 2 ]] || die "--since needs a value"; since=$2; shift 2 ;;
      *) out=$1; shift ;;
    esac
  done
  [[ -z $since || $since =~ ^[0-9]{4}-[0-9]{2}-[0-9]{2}([T ][0-9:.]+Z?)?$ ]] || die "--since: not a date: $since"
  here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
  fleet=$(cd "$proj/.fleet" 2>/dev/null && pwd) || die "no .fleet/ in $proj (is it a fleet project?)"
  [[ -f $fleet/telemetry.db ]] || die "no telemetry recorded yet ($fleet/telemetry.db)"

  data=$(sqlite3 -readonly "$fleet/telemetry.db" \
           "ATTACH 'file:$fleet/ledger.db?mode=ro' AS l" \
           ".parameter set :since \"'$since'\"" ".read $here/cost.sql")
  data=$(jq --arg project "$(basename "$(cd "$proj" && pwd)")" --arg now "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
           '. + {project: $project, generated: $now}' <<<"$data")

  if [[ -z $out ]]; then
    mkdir -p "$proj/exports"
    out="$proj/exports/fleet-spend_$(date -u +%Y-%m-%d).html"
  fi
  # `</` escaped so no ticket text can close the data's <script>; ENVIRON, as `awk -v` unescapes.
  DATA=${data//<\//<\\/} awk '$0 == "__DATA__" { print ENVIRON["DATA"]; next } { print }' \
    "$here/../template.html" > "$out"

  echo "$out"
  jq '{since, first, last, usd, last_24h, last_7d, done, by_role: [.by_role[] | {role, usd, subagents}]}' <<<"$data"
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then main "$@"; fi
