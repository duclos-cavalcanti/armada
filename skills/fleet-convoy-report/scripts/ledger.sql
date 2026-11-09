-- The ledger facts `fleetc context` leaves out, as one JSON object: the convoy's header, every
-- ticket's title and blockers (done ones too), each ticket's latest MR, the tickets worked by hand,
-- and the open proposals naming the convoy. `:convoy` is its key.
SELECT json_object(
  'convoy', (SELECT json_object('key', key, 'no', no, 'title', title, 'opened', created_at, 'closed', ended_at)
             FROM convoys WHERE key = :convoy),
  'titles', (SELECT json_group_object(key, title) FROM tickets),
  'blockers', (SELECT json_group_object(ticket, json(keys))
               FROM (SELECT ticket, json_group_array(blocked_by) AS keys FROM ticket_blocks GROUP BY ticket)),
  -- A ship that ended without a merge snapshot was closed (its snapshot froze at draft/open).
  'mrs', (SELECT json_group_object(s.ticket, json_object('iid', s.iid, 'url', s.url, 'state',
                   CASE WHEN s.ended_at IS NOT NULL AND coalesce(ss.state, 'open') IN ('draft', 'open')
                        THEN 'closed' ELSE coalesce(ss.state, 'open') END))
          FROM ships s LEFT JOIN ship_snapshots ss ON ss.ship_id = s.id
          WHERE s.ticket IS NOT NULL AND s.id = (SELECT max(id) FROM ships WHERE ticket = s.ticket)),
  -- By hand: marked for a hand, dispatched to one, or worked by one.
  'manual', (SELECT json_group_array(ticket) FROM (
               SELECT ticket FROM hand_tickets
               UNION SELECT ticket FROM orders WHERE auto_impl = 0
               UNION SELECT ticket FROM sessions WHERE kind = 'hand' AND ticket IS NOT NULL)),
  -- Outside the convoy until accepted: never in its count or grade.
  'proposals', (SELECT json_group_array(json_object('id', id, 'key', key, 'title', title, 'from', from_ticket))
                FROM (SELECT p.id, p.existing AS key, coalesce(t.title, p.title) AS title, p.from_ticket
                      FROM proposals p LEFT JOIN tickets t ON t.key = p.existing
                      WHERE p.ended_at IS NULL AND p.convoy = :convoy ORDER BY p.id))
);
