# The report's data: `fleetc context --convoy` (each ticket's derived facts) joined with the
# ledger's (ledger.sql), staged, flagged and graded. Inputs: $ctx, $led, $url (the tracker's link
# with `{key}` for the ticket's key; "" for none), $now, $project.

# A ticket's stage on the line; cancelled leaves the count.
def stage: {done: "done", in_qa: "qa", in_progress: "progress"}[.status // ""] // "todo";
# The grade's weights: each stage crossed counts the same.
def weight: {todo: 0, progress: (1 / 3), qa: (2 / 3), done: 1}[.];

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
| ($tickets | length) as $n
| {
    project: $project,
    generated: $now,
    ticket_url: (if $url == "" then null else $url end),
    convoy: ($led.convoy + {key: $ctx.convoy, plan: $ctx.plan}),
    total: $n,
    cancelled: ($ctx.tickets | map(select(.status == "cancelled")) | length),
    grade: (if $n == 0 then 0 else ($tickets | map(.stage | weight) | add) * 100 / $n | round end),
    counts: {
      done: ($tickets | map(select(.stage == "done")) | length),
      qa: ($tickets | map(select(.stage == "qa")) | length),
      progress: ($tickets | map(select(.stage == "progress")) | length),
      todo: ($tickets | map(select(.stage == "todo")) | length),
      blocked: ($tickets | map(select(.blocked and .stage != "done")) | length),
      anchored: ($tickets | map(select(.held != null and .stage != "done")) | length),
      manual: ($tickets | map(select(.manual)) | length),
      escalated: ($tickets | map(select(.escalated)) | length)
    },
    tickets: $tickets
  }
