// Orthogonal, obstacle-avoiding edge routing — a compact port of the grid
// router used by the diagram-studio export. Edges are routed as right-angle
// polylines that steer *around* node rectangles instead of cutting through
// them, which is what makes a dense architecture diagram read cleanly.
//
// Approach: build a sparse grid from the x/y lines of every (padded) obstacle
// plus the two endpoints, keep the intersections that aren't inside a box,
// connect orthogonal neighbours whose segment is clear, then Dijkstra from
// source to target minimising distance + a per-turn bend penalty.

const uniq = (a) => [...new Set(a)]

function stub(p, dir, d) {
  if (dir === 'left') return { x: p.x - d, y: p.y }
  if (dir === 'right') return { x: p.x + d, y: p.y }
  if (dir === 'top') return { x: p.x, y: p.y - d }
  return { x: p.x, y: p.y + d }
}

const strictlyInside = (x, y, o) => x > o.left && x < o.right && y > o.top && y < o.bottom

function hBlocked(y, x1, x2, obs) {
  const lo = Math.min(x1, x2), hi = Math.max(x1, x2)
  return obs.some((o) => y > o.top && y < o.bottom && hi > o.left && lo < o.right)
}
function vBlocked(x, y1, y2, obs) {
  const lo = Math.min(y1, y2), hi = Math.max(y1, y2)
  return obs.some((o) => x > o.left && x < o.right && hi > o.top && lo < o.bottom)
}

// penalty for a candidate segment vs the segments already claimed by earlier
// edges — crossings cost a flat amount, running parallel in the same lane costs
// per-pixel of overlap. This is what nudges edges onto separate tracks.
function segPenalty(axis, p1, p2, occ, cross, lane) {
  let pen = 0
  for (const o of occ) {
    const oAxis = o.y1 === o.y2 ? 0 : 1
    if (oAxis !== axis) {
      if (axis === 0) {
        const y = p1.y, x = o.x1
        if (x > Math.min(p1.x, p2.x) && x < Math.max(p1.x, p2.x) && y > Math.min(o.y1, o.y2) && y < Math.max(o.y1, o.y2)) pen += cross
      } else {
        const x = p1.x, y = o.y1
        if (y > Math.min(p1.y, p2.y) && y < Math.max(p1.y, p2.y) && x > Math.min(o.x1, o.x2) && x < Math.max(o.x1, o.x2)) pen += cross
      }
    } else if (axis === 0 && Math.abs(p1.y - o.y1) < 6) {
      const ov = Math.min(Math.max(p1.x, p2.x), Math.max(o.x1, o.x2)) - Math.max(Math.min(p1.x, p2.x), Math.min(o.x1, o.x2))
      if (ov > 0) pen += ov * lane
    } else if (axis === 1 && Math.abs(p1.x - o.x1) < 6) {
      const ov = Math.min(Math.max(p1.y, p2.y), Math.max(o.y1, o.y2)) - Math.max(Math.min(p1.y, p2.y), Math.min(o.y1, o.y2))
      if (ov > 0) pen += ov * lane
    }
  }
  return pen
}

export function routeOrthogonal(sx, sy, tx, ty, obstacles, opts = {}) {
  const clear = opts.clear ?? 16
  const d = opts.stub ?? 20
  const bend = opts.bend ?? 26
  const cross = opts.crossPenalty ?? 90
  const lane = opts.lanePenalty ?? 0.5
  const a = stub({ x: sx, y: sy }, opts.sourceDir ?? 'right', d)
  const b = stub({ x: tx, y: ty }, opts.targetDir ?? 'left', d)

  const obs = obstacles.map((o) => ({
    left: o.x - clear, right: o.x + o.w + clear, top: o.y - clear, bottom: o.y + o.h + clear,
  }))
  const occ = (opts.occupied ?? []).flatMap((poly) => {
    const segs = []
    for (let i = 1; i < poly.length; i++) {
      const p = poly[i - 1], q = poly[i]
      if (p.x !== q.x || p.y !== q.y) segs.push({ x1: p.x, y1: p.y, x2: q.x, y2: q.y })
    }
    return segs
  })

  const xs = uniq([a.x, b.x, ...obs.flatMap((o) => [o.left, o.right])]).sort((p, q) => p - q)
  const ys = uniq([a.y, b.y, ...obs.flatMap((o) => [o.top, o.bottom])]).sort((p, q) => p - q)
  const xi = new Map(xs.map((v, i) => [v, i]))
  const yi = new Map(ys.map((v, i) => [v, i]))

  const pts = [], id = new Map()
  for (const x of xs) for (const y of ys) {
    if (obs.some((o) => strictlyInside(x, y, o))) continue
    id.set(x + ',' + y, pts.length); pts.push({ x, y })
  }
  const key = (x, y) => id.get(x + ',' + y)
  const s = key(a.x, a.y), t = key(b.x, b.y)
  if (s == null || t == null) return null

  // nearest clear orthogonal neighbour in each of the four directions
  const adj = pts.map(() => [])
  const stepTo = (from, line, axis) => {
    for (const nf of [1, -1]) {
      const idx = axis === 0 ? xi.get(pts[from].x) : yi.get(pts[from].y)
      const arr = axis === 0 ? xs : ys
      for (let i = idx + nf; i >= 0 && i < arr.length; i += nf) {
        const to = axis === 0 ? key(arr[i], pts[from].y) : key(pts[from].x, arr[i])
        if (to == null) continue
        const blocked = axis === 0
          ? hBlocked(pts[from].y, pts[from].x, arr[i], obs)
          : vBlocked(pts[from].x, pts[from].y, arr[i], obs)
        if (blocked) break
        const p2 = axis === 0 ? { x: arr[i], y: pts[from].y } : { x: pts[from].x, y: arr[i] }
        const pen = occ.length ? segPenalty(axis, pts[from], p2, occ, cross, lane) : 0
        adj[from].push({ to, axis, len: Math.abs(arr[i] - line) + pen })
        break
      }
    }
  }
  for (let i = 0; i < pts.length; i++) { stepTo(i, pts[i].x, 0); stepTo(i, pts[i].y, 1) }

  // Dijkstra over (node, arrival-direction). dir 2 = start (no turn penalty).
  const N = pts.length
  const cost = new Array(N * 3).fill(Infinity)
  const prev = new Array(N * 3).fill(-1)
  const start = s * 3 + 2
  cost[start] = 0
  const pq = [{ c: 0, st: start }]
  while (pq.length) {
    let m = 0
    for (let i = 1; i < pq.length; i++) if (pq[i].c < pq[m].c) m = i
    const { c, st } = pq.splice(m, 1)[0]
    if (c > cost[st]) continue
    const node = (st / 3) | 0, dir = st % 3
    for (const e of adj[node]) {
      const turn = dir !== 2 && dir !== e.axis ? bend : 0
      const nst = e.to * 3 + e.axis
      const nc = c + e.len + turn
      if (nc < cost[nst]) { cost[nst] = nc; prev[nst] = st; pq.push({ c: nc, st: nst }) }
    }
  }

  let best = -1, bc = Infinity
  for (const dir of [0, 1]) { const st = t * 3 + dir; if (cost[st] < bc) { bc = cost[st]; best = st } }
  if (best < 0) return null

  const grid = []
  for (let st = best; st !== -1; st = prev[st]) grid.push(pts[(st / 3) | 0])
  grid.reverse()

  return [{ x: sx, y: sy }, ...grid, { x: tx, y: ty }]
}

export function simplify(points) {
  const out = []
  for (const p of points) {
    const last = out[out.length - 1]
    if (last && last.x === p.x && last.y === p.y) continue
    out.push(p)
  }
  const res = []
  for (let i = 0; i < out.length; i++) {
    if (i === 0 || i === out.length - 1) { res.push(out[i]); continue }
    const a = out[i - 1], b = out[i], c = out[i + 1]
    const collinear = (a.x === b.x && b.x === c.x) || (a.y === b.y && b.y === c.y)
    if (!collinear) res.push(b)
  }
  return res
}

const dist = (a, b) => Math.abs(a.x - b.x) + Math.abs(a.y - b.y)

// point halfway along a polyline (by arc length) — where an edge label sits
export function polylineMidpoint(points) {
  let total = 0
  for (let i = 1; i < points.length; i++) total += dist(points[i - 1], points[i])
  let half = total / 2
  for (let i = 1; i < points.length; i++) {
    const seg = dist(points[i - 1], points[i])
    if (half <= seg) {
      const t = seg ? half / seg : 0
      return { x: points[i - 1].x + (points[i].x - points[i - 1].x) * t, y: points[i - 1].y + (points[i].y - points[i - 1].y) * t }
    }
    half -= seg
  }
  return points[points.length - 1] ?? { x: 0, y: 0 }
}
const towards = (p, q, r) => {
  const len = dist(p, q) || 1
  return { x: p.x + ((q.x - p.x) / len) * r, y: p.y + ((q.y - p.y) / len) * r }
}

// right-angle polyline with small rounded corners
export function roundedPath(points, r = 8) {
  if (points.length < 2) return ''
  let d = `M ${points[0].x} ${points[0].y}`
  for (let i = 1; i < points.length - 1; i++) {
    const p0 = points[i - 1], p = points[i], p1 = points[i + 1]
    const rr = Math.min(r, dist(p0, p) / 2, dist(p1, p) / 2)
    const a = towards(p, p0, rr), b = towards(p, p1, rr)
    d += ` L ${a.x} ${a.y} Q ${p.x} ${p.y} ${b.x} ${b.y}`
  }
  const last = points[points.length - 1]
  d += ` L ${last.x} ${last.y}`
  return d
}
