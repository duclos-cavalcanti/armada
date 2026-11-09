-- The fleet's spend, as one JSON object, from the telemetry DB (main) and the ledger (attached as
-- `l`). `:since` is the window's start (RFC3339 or a date; '' = all time). A request with no role
-- is a one-off `claude` (the operator's own sessions): `interactive`, as `fleetc cost` has it. A
-- request made by a subagent carries its `agent_id`, and counts to the role that spawned it.
WITH req AS (
  SELECT ts, substr(ts, 1, 10) AS day, coalesce(role, 'interactive') AS role, ticket, session_id,
         model, cost_usd AS usd, json_extract(attrs_json, '$.agent_id') IS NOT NULL AS by_subagent,
         json_extract(attrs_json, '$.input_tokens') AS input,
         json_extract(attrs_json, '$.cache_read_tokens') AS cache_read,
         json_extract(attrs_json, '$.cache_creation_tokens') AS cache_write,
         json_extract(attrs_json, '$.output_tokens') AS output
  FROM telemetry
  WHERE event = 'claude_code.api_request' AND cost_usd IS NOT NULL AND ts >= :since
),
-- A ticket's convoy: the one it was added to, else its epic's, when the fleet opened it as one (an
-- ally's epic isn't). A captain's ticket is its convoy.
conv AS (
  SELECT key AS ticket, key AS convoy FROM l.convoys
  UNION ALL
  SELECT t.key, coalesce(m.convoy, t.convoy) FROM l.tickets t
  LEFT JOIN l.convoy_members m ON m.ticket = t.key
  WHERE t.key NOT IN (SELECT key FROM l.convoys)
    AND coalesce(m.convoy, t.convoy) IN (SELECT key FROM l.convoys)
),
attributed AS (
  SELECT req.*, conv.convoy FROM req LEFT JOIN conv ON conv.ticket = req.ticket
),
top_tickets AS (
  SELECT ticket, sum(usd) AS usd FROM attributed
  WHERE ticket IS NOT NULL AND ticket NOT IN (SELECT key FROM l.convoys)
  GROUP BY ticket ORDER BY usd DESC LIMIT 10
),
-- Spend recorded on tickets now Done: what a landed ticket costs.
done AS (
  SELECT sum(r.usd) AS usd, count(DISTINCT r.ticket) AS tickets
  FROM req r JOIN l.tickets t ON t.key = r.ticket WHERE t.status = 'done'
)
SELECT json_object(
  'since', :since,
  'first', (SELECT min(ts) FROM req),
  'last', (SELECT max(ts) FROM req),
  'usd', (SELECT coalesce(sum(usd), 0) FROM req),
  'requests', (SELECT count(*) FROM req),
  'sessions', (SELECT count(DISTINCT session_id) FROM req),
  'days', (SELECT count(DISTINCT day) FROM req),
  'last_24h', (SELECT coalesce(sum(usd), 0) FROM req WHERE ts >= strftime('%Y-%m-%dT%H:%M:%SZ', 'now', '-1 day')),
  'last_7d', (SELECT coalesce(sum(usd), 0) FROM req WHERE ts >= strftime('%Y-%m-%dT%H:%M:%SZ', 'now', '-7 days')),
  'done', (SELECT json_object('usd', coalesce(usd, 0), 'tickets', tickets) FROM done),
  'tokens', (SELECT json_object('input', sum(input), 'cache_read', sum(cache_read),
                                'cache_write', sum(cache_write), 'output', sum(output)) FROM req),
  'by_role', (SELECT json_group_array(json_object('role', role, 'usd', usd, 'subagents', sub, 'requests', n)) FROM
               (SELECT role, sum(usd) AS usd, sum(CASE WHEN by_subagent THEN usd ELSE 0 END) AS sub, count(*) AS n
                FROM req GROUP BY role ORDER BY usd DESC)),
  'by_model', (SELECT json_group_array(json_object('model', model, 'usd', usd, 'requests', n)) FROM
               (SELECT coalesce(model, '?') AS model, sum(usd) AS usd, count(*) AS n FROM req
                GROUP BY 1 ORDER BY usd DESC)),
  'by_day', (SELECT json_group_array(json_object('day', day, 'role', role, 'usd', usd)) FROM
              (SELECT day, role, sum(usd) AS usd FROM req GROUP BY day, role ORDER BY day)),
  'by_convoy', (SELECT json_group_array(json_object('convoy', a.convoy, 'no', c.no, 'title',
                  coalesce(c.title, t.title), 'role', a.role, 'usd', a.usd)) FROM
                 (SELECT convoy, role, sum(usd) AS usd FROM attributed GROUP BY convoy, role) a
                 LEFT JOIN l.convoys c ON c.key = a.convoy LEFT JOIN l.tickets t ON t.key = a.convoy),
  'by_ticket', (SELECT json_group_array(json_object('ticket', a.ticket, 'title', t.title, 'status',
                  t.status, 'convoy', a.convoy, 'role', a.role, 'usd', a.usd)) FROM
                 (SELECT ticket, convoy, role, sum(usd) AS usd FROM attributed
                  WHERE ticket IN (SELECT ticket FROM top_tickets) GROUP BY ticket, convoy, role) a
                 LEFT JOIN l.tickets t ON t.key = a.ticket)
);
