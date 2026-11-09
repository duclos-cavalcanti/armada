# The report's data: `fleetc context --convoy` (each ticket's derived facts) joined with the
# ledger's (ledger.sql), staged, flagged and graded. Inputs: $ctx, $led, $url (the tracker's link
# with `{key}` for the ticket's key; "" for none), $now, $project.

# A ticket's stage on the line; cancelled leaves the count.
def stage: {done: "done", in_qa: "qa", in_progress: "progress"}[.status // ""] // "todo";
# The grade's weights, in thirds: each stage crossed counts the same.
def weight: {todo: 0, progress: 1, qa: 2, done: 3}[.];
# How far a set of tickets has moved, in %.
def grade: if length == 0 then 0 else (map(.stage | weight) | add) * 100 / (3 * length) | round end;
# A set of keys and every key it waits on, transitively, through $by (key → blocker keys).
def closure($by): (. + [.[] | $by[.] // [] | .[]] | unique) as $n
  | if ($n | length) == length then . else $n | closure($by) end;

($led.manual) as $manual
| ($ctx.tickets | map(select(.status != "cancelled"))) as $live
| ($live | map(
    .key as $k
    | {
        key: $k,
        title: ($led.titles[$k] // ""),
        stage: stage,
        status: (.status // "unknown"),
        cog: .live_cog,
        mr: $led.mrs[$k],
        blocked_by: .waiting_on,
        blocked: ((.waiting_on | length) > 0 or .status == "blocked"),
        held: .held,
        manual: (.hand or .live_cog == "hand" or any($manual[]; . == $k)),
        escalated: .inspect
      })) as $tickets
# The critical path: the keys on the plan's `**Critical path**` line, and every ticket of the convoy
# they wait on, done or not; none when the plan names none.
| ($tickets | map(.key)) as $keys
| ($led.blockers | map_values(map(select(IN($keys[]))))) as $by
| ([($ctx.plan // "") | splits("\n") | select(test("^\\s*[-*]\\s*\\*\\*critical path"; "i"))][0] // ""
   | [scan("[A-Z][A-Z0-9]*-[0-9]+") | select(IN($keys[]))]) as $named
| ($named | closure($by)) as $path
| ($tickets | map(.critical = (.key | IN($path[])))) as $tickets
| ($tickets | length) as $n
| {
    project: $project,
    generated: $now,
    ticket_url: (if $url == "" then null else $url end),
    convoy: ($led.convoy + {key: $ctx.convoy, plan: $ctx.plan}),
    total: $n,
    cancelled: ($ctx.tickets | map(select(.status == "cancelled")) | length),
    grade: ($tickets | grade),
    counts: {
      done: ($tickets | map(select(.stage == "done")) | length),
      qa: ($tickets | map(select(.stage == "qa")) | length),
      progress: ($tickets | map(select(.stage == "progress")) | length),
      todo: ($tickets | map(select(.stage == "todo")) | length),
      blocked: ($tickets | map(select(.blocked and .stage != "done")) | length),
      anchored: ($tickets | map(select(.held != null and .stage != "done")) | length),
      manual: ($tickets | map(select(.manual)) | length),
      escalated: ($tickets | map(select(.escalated)) | length),
      proposed: ($led.proposals | length)
    },
    critical: (if $named == [] then null else ($tickets | map(select(.critical))) as $cp | {
      named: $named,
      total: ($cp | length),
      grade: ($cp | grade),
      counts: ($cp | reduce .[].stage as $s ({done: 0, qa: 0, progress: 0, todo: 0}; .[$s] += 1))
    } end),
    tickets: $tickets,
    proposals: $led.proposals
  }
