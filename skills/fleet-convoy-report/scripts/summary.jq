# The report's data: `fleetc context --convoy` (each ticket's derived facts) joined with the
# ledger's (ledger.sql), staged, flagged and graded. Inputs: $ctx, $led, $url (the tracker's link
# with `{key}` for the ticket's key; "" for none), $now, $project.

# A ticket's stage on the line; cancelled leaves the count.
def stage: {done: "done", in_qa: "qa", in_progress: "progress"}[.status // ""] // "todo";
# The grade's weights, in thirds: each stage crossed counts the same.
def weight: {todo: 0, progress: 1, qa: 2, done: 3}[.];
# How far a set of tickets has moved, in %.
def grade: if length == 0 then 0 else (map(.stage | weight) | add) * 100 / (3 * length) | round end;

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
        escalated: .inspect,
        # On the critical path, as fleetd derives it: the convoy's goal and every ticket it waits on.
        critical: (.critical // false)
      })) as $tickets
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
    critical: (if $ctx.goal == null then null else ($tickets | map(select(.critical))) as $cp | {
      goal: $ctx.goal,
      total: ($cp | length),
      grade: ($cp | grade),
      counts: ($cp | reduce .[].stage as $s ({done: 0, qa: 0, progress: 0, todo: 0}; .[$s] += 1))
    } end),
    escalations: $led.escalations,
    tickets: $tickets,
    proposals: $led.proposals
  }
