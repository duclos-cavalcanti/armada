//! fleetc — the fleet client CLI (PLAN §2.1): the sanctioned write path. Verbs are
//! validated locally (fail-fast), serialized to `wire` DTOs, and POSTed to `fleetd`
//! (the single writer) over its UDS. This increment ships `status` — the ported
//! status-drop channel that replaces `snapshot_write`.

use anyhow::{anyhow, bail, Result};
use domain::{CogState, CrewKind};

fn main() {
    if let Err(e) = run() {
        eprintln!("fleetc: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("status") => status(&args[1..]),
        Some("dispatch") => dispatch(&args[1..]),
        Some("hand") => hand(&args[1..]),
        Some("convoy") => convoy(&args[1..]),
        Some("anchor") => anchor(&args[1..]),
        Some("weigh") => weigh(&args[1..]),
        Some("sink") => sink(&args[1..]),
        Some("send") => send(&args[1..]),
        Some("hold") => hold(&args[1..]),
        Some("unhold") => unhold(&args[1..]),
        Some("packet") => packet(&args[1..]),
        Some("context") => context(&args[1..]),
        Some("plan") => plan(&args[1..]),
        Some("transcript") => transcript(&args[1..]),
        Some("review") => review(&args[1..]),
        Some("rebase") => rebase(&args[1..]),
        Some("settings") => settings(&args[1..]),
        Some("cost") => cost(&args[1..]),
        _ => {
            eprintln!(
                "usage:\n  \
                 fleetc status   --cog <worker|sailor|hand> --ticket <KEY-N> --state <state> [--last-action <text>]\n  \
                 fleetc dispatch --ticket <KEY-N> --ac <summary> [--hand] [--hold] [--fork-from <ref>] [--worktree <path>] [--park-at <where>]\n  \
                 fleetc hand     [--worktree <path>]   (ticketless operator-driven hand in a worktree; defaults to cwd)\n  \
                 fleetc convoy   <open|close> <KEY> [--title <text>]\n  \
                 fleetc anchor   --cog <kind> [--ticket <KEY-N>|--convoy <KEY>]   (graceful stop & hold)\n  \
                 fleetc weigh    --cog <kind> [--ticket <KEY-N>|--convoy <KEY>]   (resume)\n  \
                 fleetc sink     --cog <kind> [--ticket <KEY-N>|--convoy <KEY>]   (scuttle — hard teardown)\n  \
                 fleetc send     --cog <kind> [--ticket <KEY-N>|--convoy <KEY>] <message>   (message into the cog's pane)\n  \
                 fleetc hold     <KEY-N> [--reason <why>]  (hold off the dispatch frontier)\n  \
                 fleetc unhold   <KEY-N>          (release back onto the frontier)\n  \
                 fleetc packet   <KEY-N>          (the cog's work-packet: order + ship + ticket)\n  \
                 fleetc context  --convoy <KEY> [--format table]   (a captain's convoy slice)\n  \
                 fleetc plan     [--db <path>]     (fleetd's latest reconcile plan — the parity lens)\n  \
                 fleetc transcript --cog <kind> [--ticket <KEY-N>|--convoy <KEY>] [--open]   (resolve a cog's transcript)\n  \
                 fleetc review   --iid <N>         (an MR's fresh review surface: open-thread count + floating notes)\n  \
                 fleetc rebase   --iid <N>         (server-side rebase an MR)\n  \
                 fleetc settings [list | set <key> <value>]   (live config levers; no restart)\n  \
                 fleetc cost     [--since <RFC3339>] [--db <path>]"
            );
            std::process::exit(2);
        }
    }
}

/// `fleetc status` — report a crew member's state. The per-cog enum is the lint: an
/// invalid cog/state fails HERE, before anything touches the wire.
fn status(args: &[String]) -> Result<()> {
    let (mut cog, mut ticket, mut state, mut last_action) = (None, None, None, None);
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        let mut take = || it.next().cloned().ok_or_else(|| anyhow!("{flag} needs a value"));
        match flag.as_str() {
            "--cog" => cog = Some(take()?),
            "--ticket" => ticket = Some(take()?),
            "--state" => state = Some(take()?),
            "--last-action" => last_action = Some(take()?),
            other => bail!("unknown flag: {other}"),
        }
    }
    let cog = cog.ok_or_else(|| anyhow!("--cog required"))?;
    let ticket = ticket_key(&ticket.ok_or_else(|| anyhow!("--ticket required"))?, "status")?;
    let state = state.ok_or_else(|| anyhow!("--state required"))?;

    let kind = CrewKind::parse(&cog).ok_or_else(|| anyhow!("invalid --cog: {cog}"))?;
    let parsed = CogState::parse(kind, &state)
        .ok_or_else(|| anyhow!("invalid state '{state}' for cog '{cog}'"))?;

    submit("/status", &wire::StatusReport {
        cog: kind.as_str().into(),
        ticket,
        state: parsed.as_str().into(),
        last_action,
    })
}

/// `fleetc dispatch` — submit an order intent (the dispatch packet) to the queue, for
/// reconcile to spawn. Defaults are the normal path: worker + auto-merge.
fn dispatch(args: &[String]) -> Result<()> {
    let (mut ticket, mut ac, mut fork_from, mut park_at, mut worktree) = (None, None, None, None, None);
    let (mut auto_impl, mut auto_review) = (true, true);
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        let mut take = || it.next().cloned().ok_or_else(|| anyhow!("{flag} needs a value"));
        match flag.as_str() {
            "--ticket" => ticket = Some(take()?),
            "--ac" => ac = Some(take()?),
            "--fork-from" => fork_from = Some(take()?),
            "--worktree" => worktree = Some(take()?),
            "--park-at" => park_at = Some(take()?),
            "--hand" => auto_impl = false,   // operator-driven, not a worker
            "--hold" => auto_review = false,  // held in draft for review, not auto-merge
            other => bail!("unknown flag: {other}"),
        }
    }
    let ticket = ticket_key(&ticket.ok_or_else(|| anyhow!("--ticket required"))?, "dispatch")?;
    // A worker implements against its AC (the goal); a hand is operator-driven with no
    // autonomous goal, so --ac is optional for --hand — default it to a legible sentinel.
    let ac_summary = match (ac, auto_impl) {
        (Some(s), _) => s,
        (None, false) => "operator-driven — no autonomous goal".into(),
        (None, true) => bail!("--ac <summary> required (a worker needs a goal; use --hand for an operator-driven cog)"),
    };

    submit("/dispatch", &wire::DispatchOrder {
        ticket,
        auto_impl,
        auto_review,
        fork_from: fork_from.unwrap_or_else(|| "origin/master".into()),
        ac_summary,
        park_at,
        worktree_path: worktree,
    })
}

/// `fleetc hand [--worktree <path>]` — launch a TICKETLESS operator-driven hand in a worktree
/// (defaults to the current dir). No ticket, no order — a tracked scratch cog, distinct from the
/// ticketed `dispatch --hand`. You drive it; reconcile leaves it alone.
fn hand(args: &[String]) -> Result<()> {
    let mut worktree = None;
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        let mut take = || it.next().cloned().ok_or_else(|| anyhow!("{flag} needs a value"));
        match flag.as_str() {
            "--worktree" => worktree = Some(take()?),
            other => bail!("unknown flag: {other}"),
        }
    }
    let worktree_path = match worktree {
        Some(w) => w,
        None => std::env::current_dir()?.to_string_lossy().into_owned(),
    };
    submit("/hand", &wire::HandLaunch { worktree_path })
}

/// `fleetc convoy open <KEY> [--title <t>]` — the operator opens a convoy (→ a captain gets
/// staffed). `fleetc convoy close <KEY>` — the captain's "all delivered" sign-off (→ reconcile
/// reaps the captain next tick).
fn convoy(args: &[String]) -> Result<()> {
    match args.first().map(String::as_str) {
        Some("open") => {
            let (mut key, mut title) = (None, None);
            let mut it = args[1..].iter();
            while let Some(a) = it.next() {
                match a.as_str() {
                    "--title" => title = Some(it.next().cloned().ok_or_else(|| anyhow!("--title needs a value"))?),
                    other => key = Some(other.to_string()),
                }
            }
            let key = key.ok_or_else(|| anyhow!("convoy open needs <KEY>"))?;
            submit("/convoy-open", &wire::ConvoyCmd { key, title })
        }
        Some("close") => {
            let key = args.get(1).cloned().ok_or_else(|| anyhow!("convoy close needs <KEY>"))?;
            submit("/convoy-close", &wire::ConvoyCmd { key, title: None })
        }
        _ => bail!("usage: fleetc convoy <open|close> <KEY> [--title <text>]"),
    }
}

/// `fleetc anchor --cog <kind> [--ticket|--convoy]` — drop anchor on the cog: a graceful stop & hold
/// (it checkpoints; reconcile won't respawn it). To tear a cog down hard, use `sink`.
fn anchor(args: &[String]) -> Result<()> {
    submit("/anchor", &require_cog_target(args, "anchor")?)
}

/// `fleetc weigh --cog <kind> [--ticket|--convoy]` — weigh anchor (get under way): clear the hold.
fn weigh(args: &[String]) -> Result<()> {
    submit("/weigh", &require_cog_target(args, "weigh")?)
}

/// `fleetc sink --cog <kind> [--ticket|--convoy]` — scuttle the cog: a deliberate, destructive
/// teardown (end the session + kill the tmux). Its own verb — sinking a ship is not casual.
fn sink(args: &[String]) -> Result<()> {
    submit("/sink", &require_cog_target(args, "sink")?)
}

/// `fleetc hold <KEY-N> --reason <why>` — hold the ticket off the dispatch frontier (the captain's
/// groom-gate decision, or an operator lever). `--reason` rides on the `TicketHeld` event.
fn hold(args: &[String]) -> Result<()> {
    let ticket = require_ticket(args, "hold")?;
    let mut reason = None;
    let mut it = args[1..].iter();
    while let Some(flag) = it.next() {
        match flag.as_str() {
            "--reason" => reason = Some(it.next().cloned().ok_or_else(|| anyhow!("--reason needs a value"))?),
            other => bail!("unknown flag: {other}"),
        }
    }
    submit("/hold", &wire::HoldCmd { ticket, reason })
}

/// `fleetc unhold <KEY-N>` — release a held ticket back onto the frontier.
fn unhold(args: &[String]) -> Result<()> {
    let ticket = require_ticket(args, "unhold")?;
    submit("/unhold", &wire::HoldCmd { ticket, reason: None })
}

/// `fleetc packet <KEY-N>` — read the cog's composed work-packet (order + ship + snapshot +
/// ticket) from fleetd and print it as JSON. The sailor's boot read; also handy for an operator.
fn packet(args: &[String]) -> Result<()> {
    let ticket = require_ticket(args, "packet")?;
    let socket = std::env::var("FLEET_SOCK").unwrap_or_else(|_| wire::default_socket());
    let (code, body) = wire::get_json(&socket, &format!("/packet?ticket={ticket}"))?;
    if code == 200 {
        println!("{body}");
        Ok(())
    } else {
        bail!("fleetd ({code}): {body}")
    }
}

/// `fleetc context --convoy <KEY> [--format table]` — read a captain's convoy slice (each ticket
/// with `ready`/`dispatched`). JSON by default (what the captain parses); `--format table` renders
/// a human view for an operator.
fn context(args: &[String]) -> Result<()> {
    let (mut convoy, mut table) = (None, false);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--convoy" => convoy = Some(it.next().cloned().ok_or_else(|| anyhow!("--convoy needs a value"))?),
            "--format" => {
                table = it.next().map(|f| f == "table").unwrap_or(false);
            }
            other => bail!("unknown flag: {other}"),
        }
    }
    let convoy = convoy.ok_or_else(|| anyhow!("context needs --convoy <KEY>"))?;
    let socket = std::env::var("FLEET_SOCK").unwrap_or_else(|_| wire::default_socket());
    let (code, body) = wire::get_json(&socket, &format!("/context?convoy={convoy}"))?;
    if code != 200 {
        bail!("fleetd ({code}): {body}");
    }
    if table {
        print!("{}", render_convoy_table(&body)?);
    } else {
        println!("{body}");
    }
    Ok(())
}

/// Render a `ConvoyView` JSON as a human table (generic — no domain dependency).
fn render_convoy_table(json: &str) -> Result<String> {
    let v: serde_json::Value = serde_json::from_str(json)?;
    let tickets = v.get("tickets").and_then(|t| t.as_array()).ok_or_else(|| anyhow!("no tickets in view"))?;
    let cell = |t: &serde_json::Value, k: &str| -> String {
        match t.get(k) {
            Some(serde_json::Value::String(s)) => s.clone(),
            Some(serde_json::Value::Null) | None => "-".into(),
            Some(other) => other.to_string(),
        }
    };
    let mut out = format!("{:<12} {:<12} {:<8} {:<10} {:<8} {}\n", "TICKET", "STATUS", "CLASS", "DISPATCH", "COG", "SHIP");
    for t in tickets {
        out += &format!(
            "{:<12} {:<12} {:<8} {:<10} {:<8} {}\n",
            cell(t, "key"),
            cell(t, "status"),
            cell(t, "classification"),
            cell(t, "dispatched"),
            cell(t, "live_cog"),
            cell(t, "ship_state"),
        );
    }
    Ok(out)
}

/// The required `<KEY-N>` positional. Rejects a flag-looking arg (a mistyped flag silently
/// swallowed as a "ticket" is a no-op downstream — the bug this guards) and a non-ticket string,
/// so operator error fails loudly instead of no-op'ing.
fn require_ticket(args: &[String], verb: &str) -> Result<String> {
    let t = args.first().ok_or_else(|| anyhow!("{verb} needs <KEY-N>"))?;
    if t.starts_with('-') {
        bail!("{verb}: unexpected flag {t:?} — expected a ticket <KEY-N>");
    }
    ticket_key(t, verb)
}

/// Validate a ticket key at the edge — the domain's `<PREFIX>-<n>` parse, tagged with the verb.
fn ticket_key(t: &str, verb: &str) -> Result<String> {
    domain::TicketKey::parse(t).map(|k| k.0).map_err(|e| anyhow!("{verb}: {e}"))
}

/// Parse + validate the uniform cog target for a control verb (§A): `--cog <kind>` is required, and
/// the key is per-cog — worker/sailor/hand need `--ticket`, a captain `--convoy`, the lookout neither.
/// A missing `--cog`, an unknown kind, or the wrong key errors here (fail-fast; fleetd re-validates).
fn require_cog_target(args: &[String], verb: &str) -> Result<wire::CogTarget> {
    let (mut cog, mut ticket, mut convoy) = (None, None, None);
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        let mut take = || it.next().cloned().ok_or_else(|| anyhow!("{flag} needs a value"));
        match flag.as_str() {
            "--cog" => cog = Some(take()?),
            "--ticket" => ticket = Some(take()?),
            "--convoy" => convoy = Some(take()?),
            other => bail!("{verb}: unknown flag {other:?}"),
        }
    }
    let cog = cog.ok_or_else(|| anyhow!("{verb} needs --cog <worker|sailor|hand|captain|lookout>"))?;
    validate_cog_target(&cog, ticket.as_deref(), convoy.as_deref(), verb)?;
    Ok(wire::CogTarget { cog, ticket, convoy })
}

/// Validate a cog target's shape (shared by the control verbs + `send`): worker/sailor/hand need a
/// `--ticket`, a captain a `--convoy`, the lookout neither. An unknown kind or the wrong key errors.
fn validate_cog_target(cog: &str, ticket: Option<&str>, convoy: Option<&str>, verb: &str) -> Result<()> {
    match cog {
        "worker" | "sailor" | "hand" => {
            let t = ticket.ok_or_else(|| anyhow!("{verb} --cog {cog} needs --ticket <KEY-N>"))?;
            ticket_key(t, verb)?;
            if convoy.is_some() {
                bail!("{verb} --cog {cog}: --convoy is not valid — use --ticket");
            }
        }
        "captain" => {
            if convoy.is_none() {
                bail!("{verb} --cog captain needs --convoy <KEY>");
            }
            if ticket.is_some() {
                bail!("{verb} --cog captain: --ticket is not valid — use --convoy");
            }
        }
        "lookout" => {
            if ticket.is_some() || convoy.is_some() {
                bail!("{verb} --cog lookout takes no key (it's the singleton)");
            }
        }
        other => bail!("{verb}: unknown --cog {other:?}"),
    }
    Ok(())
}

/// `fleetc send --cog <kind> [--ticket|--convoy] "<message>"` — push an operator message into the
/// target cog's tmux pane (typed + submitted as a prompt). The target is the same per-cog addressing
/// as the control verbs; the message is the lone positional. No broadcast — serialize sends if needed.
fn send(args: &[String]) -> Result<()> {
    submit("/send", &parse_send(args)?)
}

/// Parse `send`'s args into the wire DTO — the cog target (flags) + the message (lone positional),
/// validated. Split out from `send` so the parse is unit-testable without the socket.
fn parse_send(args: &[String]) -> Result<wire::SendMsg> {
    let (mut cog, mut ticket, mut convoy, mut message) = (None, None, None, None);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--cog" => cog = Some(it.next().cloned().ok_or_else(|| anyhow!("--cog needs a value"))?),
            "--ticket" => ticket = Some(it.next().cloned().ok_or_else(|| anyhow!("--ticket needs a value"))?),
            "--convoy" => convoy = Some(it.next().cloned().ok_or_else(|| anyhow!("--convoy needs a value"))?),
            _ => {
                if message.is_some() {
                    bail!("send: unexpected extra arg {a:?} — quote the message");
                }
                message = Some(a.clone());
            }
        }
    }
    let cog = cog.ok_or_else(|| anyhow!("send needs --cog <worker|sailor|hand|captain|lookout>"))?;
    let message = message.ok_or_else(|| anyhow!("send needs a <message> (quote it)"))?;
    validate_cog_target(&cog, ticket.as_deref(), convoy.as_deref(), "send")?;
    Ok(wire::SendMsg { cog, ticket, convoy, message })
}

/// POST a validated DTO to fleetd and turn a non-200 into a clear error.
fn submit<T: serde::Serialize>(path: &str, body: &T) -> Result<()> {
    let socket = std::env::var("FLEET_SOCK").unwrap_or_else(|_| wire::default_socket());
    let (code, resp) = wire::post_json(&socket, path, body)?;
    if code == 200 {
        Ok(())
    } else {
        bail!("fleetd rejected the request ({code}): {resp}")
    }
}

/// `fleetc plan` — show fleetd's latest reconcile plan: what it *would* do (dry-run: `planned[]`)
/// or *did* (live: `applied[]`/`failed[]`) last tick. The parity lens — read-only, so it works
/// while fleetd writes. `--db <path>` overrides the ledger location (default `$HOME/.fleet/ledger.db`,
/// or `$FLEET_LEDGER`).
fn plan(args: &[String]) -> Result<()> {
    let mut db = None;
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        match flag.as_str() {
            "--db" => db = Some(it.next().cloned().ok_or_else(|| anyhow!("--db needs a value"))?),
            other => bail!("unknown flag: {other}"),
        }
    }
    let db = db.or_else(|| std::env::var("FLEET_LEDGER").ok()).unwrap_or_else(wire::default_ledger);
    let ledger = match ledger::Ledger::open_readonly(&db) {
        Ok(l) => l,
        Err(_) => {
            println!("no ledger yet ({db})");
            return Ok(());
        }
    };
    match ledger.latest_tick()? {
        Some(tick) => print!("{}", render_plan(&tick)),
        None => println!("no tick has run yet"),
    }
    Ok(())
}

/// Render a `TickRan` event as a terse action list (generic over the payload JSON).
fn render_plan(tick: &domain::Event) -> String {
    let p = &tick.payload;
    let dry = p.get("dry_run").and_then(serde_json::Value::as_bool).unwrap_or(false);
    let mut out = format!("plan @ {}  ({})\n", tick.created_at, if dry { "dry-run" } else { "live" });
    let fmt = |a: &serde_json::Value| -> String {
        let action = a.get("action").and_then(serde_json::Value::as_str).unwrap_or("?");
        let who = a.get("ticket").or_else(|| a.get("convoy")).and_then(serde_json::Value::as_str).unwrap_or("-");
        let kind = a.get("kind").and_then(serde_json::Value::as_str).unwrap_or("");
        format!("{action:<16} {who:<12} {kind}").trim_end().to_string()
    };
    if let Some(planned) = p.get("planned").and_then(serde_json::Value::as_array) {
        if planned.is_empty() {
            out += "  (nothing to do)\n";
        }
        for a in planned {
            out += &format!("  {}\n", fmt(a));
        }
        return out;
    }
    if let Some(applied) = p.get("applied").and_then(serde_json::Value::as_array) {
        for a in applied {
            out += &format!("  ✓ {}\n", fmt(a));
        }
    }
    if let Some(failed) = p.get("failed").and_then(serde_json::Value::as_array) {
        for f in failed {
            let err = f.get("error").and_then(serde_json::Value::as_str).unwrap_or("");
            if let Some(a) = f.get("action") {
                out += &format!("  ✗ {} — {err}\n", fmt(a));
            }
        }
    }
    out
}

/// `fleetc transcript` — resolve a cog's claude transcript jsonl and print its path (`--open`
/// opens it in `$EDITOR`). Reads the ledger read-only (like `plan`), finds the cog's live session,
/// and maps it to the transcript: the minted `harness_session_id` → the canonical project dir
/// (`~/.claude/projects/<cwd-encoded>/<id>.jsonl`, authoritative), or — for a pre-capture session
/// with no stored id — the newest jsonl in that dir (convention fallback).
fn transcript(args: &[String]) -> Result<()> {
    use domain::ports::SessionRepo;
    let open = args.iter().any(|a| a == "--open");
    let rest: Vec<String> = args.iter().filter(|a| a.as_str() != "--open").cloned().collect();
    let target = require_cog_target(&rest, "transcript")?;

    let db = std::env::var("FLEET_LEDGER").ok().unwrap_or_else(wire::default_ledger);
    let ledger = ledger::Ledger::open_readonly(&db).map_err(|_| anyhow!("no ledger yet ({db})"))?;

    let kind = CrewKind::parse(&target.cog).ok_or_else(|| anyhow!("unknown --cog {:?}", target.cog))?;
    let session = SessionRepo::live(&ledger)?
        .into_iter()
        .find(|s| s.kind == kind && matches_target(s, &target))
        .ok_or_else(|| anyhow!("no live {} session for that target", target.cog))?;

    let path = transcript_path(&session)?;
    if open {
        let editor = std::env::var("EDITOR").unwrap_or_else(|_| "nvim".into());
        std::process::Command::new(editor).arg(&path).status()?;
    } else {
        println!("{}", path.display());
    }
    Ok(())
}

/// Does this live session match the cog target's key? Ticket for worker/sailor/hand, convoy for a
/// captain, nothing for the singleton lookout (kind already matched by the caller).
fn matches_target(s: &domain::Session, t: &wire::CogTarget) -> bool {
    match s.kind {
        CrewKind::Worker | CrewKind::Sailor | CrewKind::Hand => {
            s.ticket.as_ref().map(|k| &k.0) == t.ticket.as_ref()
        }
        CrewKind::Captain => s.convoy.as_ref().map(|k| &k.0) == t.convoy.as_ref(),
        CrewKind::Lookout => true,
    }
}

/// Map a live session to its transcript jsonl. Primary: the stored harness id under the canonical
/// project dir. Fallback (no stored id): the newest jsonl in that dir. Claude encodes the session's
/// REAL cwd (symlinks resolved) with `/`→`-`, so canonicalize the worktree to match its path.
fn transcript_path(s: &domain::Session) -> Result<std::path::PathBuf> {
    let home = std::env::var("HOME").map_err(|_| anyhow!("HOME unset"))?;
    let cwd = std::fs::canonicalize(&s.worktree_path)
        .unwrap_or_else(|_| std::path::PathBuf::from(&s.worktree_path));
    let encoded = cwd.to_string_lossy().replace('/', "-");
    let project_dir = std::path::Path::new(&home).join(".claude/projects").join(encoded);

    if let Some(id) = &s.harness_session_id {
        return Ok(project_dir.join(format!("{id}.jsonl")));
    }
    newest_jsonl(&project_dir)
        .ok_or_else(|| anyhow!("no transcript under {} (cog not started yet?)", project_dir.display()))
}

/// The newest `*.jsonl` in `dir` by mtime — the convention fallback when a session has no stored id.
fn newest_jsonl(dir: &std::path::Path) -> Option<std::path::PathBuf> {
    let mut jsonls: Vec<std::path::PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("jsonl"))
        .collect();
    jsonls.sort_by_key(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok());
    jsonls.pop()
}

/// `fleetc review --iid <N>` — the MR's fresh review surface from the factory's forge:
/// `{open_threads, floating_notes}`. One call replacing the bash mr-open-threads + mr-floating-notes.
/// Routes through fleetd (fresh — a cog resolving review needs current state).
fn review(args: &[String]) -> Result<()> {
    let iid = require_iid(args, "review")?;
    let socket = std::env::var("FLEET_SOCK").unwrap_or_else(|_| wire::default_socket());
    let (code, body) = wire::get_json(&socket, &format!("/review?iid={iid}"))?;
    if code != 200 {
        bail!("fleetd ({code}): {body}");
    }
    println!("{body}");
    Ok(())
}

/// `fleetc rebase --iid <N>` — server-side rebase the MR via the factory's forge (`GlabForge::rebase`).
fn rebase(args: &[String]) -> Result<()> {
    let iid = require_iid(args, "rebase")?;
    let socket = std::env::var("FLEET_SOCK").unwrap_or_else(|_| wire::default_socket());
    let (code, body) = wire::post_json(&socket, &format!("/rebase?iid={iid}"), &serde_json::json!({}))?;
    if code != 200 {
        bail!("fleetd ({code}): {body}");
    }
    println!("{body}");
    Ok(())
}

/// Parse the required `--iid <N>` from a verb's args.
fn require_iid(args: &[String], verb: &str) -> Result<i64> {
    let mut iid = None;
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        match flag.as_str() {
            "--iid" => iid = Some(it.next().cloned().ok_or_else(|| anyhow!("--iid needs a value"))?),
            other => bail!("{verb}: unknown flag {other:?}"),
        }
    }
    let iid = iid.ok_or_else(|| anyhow!("{verb} needs --iid <N>"))?;
    iid.parse().map_err(|_| anyhow!("{verb}: --iid must be an integer, got {iid:?}"))
}

/// `fleetc settings [list | set <key> <value>]` — read or overwrite the live config levers (the
/// fleet.toml overridable subset). `list` (the default) shows each lever's effective value + source;
/// `set` overwrites one lever and fleetd picks it up next tick (no restart). The key + type are
/// validated server-side before the value is absorbed.
fn settings(args: &[String]) -> Result<()> {
    match args.first().map(String::as_str) {
        Some("list") | None => settings_list(),
        Some("set") => {
            let key = args.get(1).cloned().ok_or_else(|| anyhow!("settings set needs <key> <value>"))?;
            let value = args.get(2).cloned().ok_or_else(|| anyhow!("settings set {key} needs a <value>"))?;
            submit("/settings", &wire::SettingCmd { key, value })
        }
        Some(other) => bail!("usage: fleetc settings [list | set <key> <value>]  (got {other:?})"),
    }
}

/// `fleetc settings list` — fetch every live lever's effective value + source and render a table.
fn settings_list() -> Result<()> {
    let socket = std::env::var("FLEET_SOCK").unwrap_or_else(|_| wire::default_socket());
    let (code, body) = wire::get_json(&socket, "/settings")?;
    if code != 200 {
        bail!("fleetd ({code}): {body}");
    }
    print!("{}", render_settings(&body)?);
    Ok(())
}

/// Render the `[{key, value, source}]` settings JSON as a human table (generic — no domain dep).
fn render_settings(json: &str) -> Result<String> {
    let levers: serde_json::Value = serde_json::from_str(json)?;
    let levers = levers.as_array().ok_or_else(|| anyhow!("expected a settings array"))?;
    let cell = |l: &serde_json::Value, k: &str| l.get(k).and_then(|v| v.as_str()).unwrap_or("-").to_string();
    let mut out = format!("{:<16} {:<10} {}\n", "LEVER", "VALUE", "SOURCE");
    for l in levers {
        out += &format!("{:<16} {:<10} {}\n", cell(l, "key"), cell(l, "value"), cell(l, "source"));
    }
    Ok(out)
}

/// `fleetc cost` — roll up spend by role from the telemetry DB (read-only, so it works while
/// fleetd writes). A one-off `claude` (no attribution) rolls up under `interactive`.
/// `--since <RFC3339>` windows it; default = all time.
fn cost(args: &[String]) -> Result<()> {
    let (mut db, mut since) = (None, None);
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        let mut take = || it.next().cloned().ok_or_else(|| anyhow!("{flag} needs a value"));
        match flag.as_str() {
            "--db" => db = Some(take()?),
            "--since" => since = Some(take()?),
            other => bail!("unknown flag: {other}"),
        }
    }
    let db = db.unwrap_or_else(wire::default_telemetry_db);
    let since = since.unwrap_or_default();

    let store = match ledger::telemetry::TelemetryStore::open_readonly(&db) {
        Ok(s) => s,
        Err(_) => {
            println!("no telemetry recorded yet ({db})");
            return Ok(());
        }
    };
    let rows = store.cost_by_role(&since)?;
    if rows.is_empty() {
        println!("no telemetry in range");
        return Ok(());
    }
    let total: f64 = rows.iter().map(|(_, c)| c).sum();
    for (role, cost) in &rows {
        println!("{role:<14} ${cost:>10.4}");
    }
    println!("{:-<27}", "");
    println!("{:<14} ${total:>10.4}", "TOTAL");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::{EntityKind, Event, EventKind, Seq};

    fn tick(payload: serde_json::Value) -> Event {
        Event {
            seq: Seq(1),
            created_at: "t1".into(),
            kind: EventKind::TickRan,
            entity_kind: EntityKind::Ticker,
            entity_ref: "reconcile".into(),
            payload,
        }
    }

    #[test]
    fn render_plan_lists_dry_run_actions_with_ticket_or_convoy() {
        let out = render_plan(&tick(serde_json::json!({
            "dry_run": true,
            "planned": [
                { "action": "spawn", "ticket": "MADY-1", "kind": "worker" },
                { "action": "launch_captain", "convoy": "MADY-100" },
            ]
        })));
        assert!(out.contains("(dry-run)"));
        assert!(out.contains("spawn") && out.contains("MADY-1") && out.contains("worker"));
        assert!(out.contains("launch_captain") && out.contains("MADY-100"), "convoy actions render too");
    }

    #[test]
    fn require_ticket_rejects_flags_and_non_tickets() {
        assert_eq!(require_ticket(&["MADY-123".into()], "anchor").unwrap(), "MADY-123");
        // a mistyped flag swallowed as a "ticket" is the bug — must error, not no-op.
        let e = require_ticket(&["--cog".into()], "anchor").unwrap_err().to_string();
        assert!(e.contains("unexpected flag"), "{e}");
        // typos fail loudly too.
        assert!(require_ticket(&["mady-5".into()], "sink").is_err(), "lowercase");
        assert!(require_ticket(&["MADY-".into()], "sink").is_err(), "no number");
        assert!(require_ticket(&["MADY-12x".into()], "sink").is_err(), "trailing junk");
        assert!(require_ticket(&[], "weigh").is_err(), "missing");
    }

    #[test]
    fn require_cog_target_validates_cog_and_per_cog_key() {
        let a = |args: &[&str]| {
            require_cog_target(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>(), "anchor")
        };
        // valid: each cog with its right key.
        assert_eq!(a(&["--cog", "worker", "--ticket", "MADY-1"]).unwrap().cog, "worker");
        assert_eq!(
            a(&["--cog", "captain", "--convoy", "MADY-100"]).unwrap().convoy.as_deref(),
            Some("MADY-100")
        );
        assert!(a(&["--cog", "lookout"]).unwrap().ticket.is_none());
        // errors — all loud, never silent.
        assert!(a(&["--ticket", "MADY-1"]).is_err(), "missing --cog");
        assert!(a(&["--cog", "worker"]).is_err(), "worker needs --ticket");
        assert!(a(&["--cog", "captain"]).is_err(), "captain needs --convoy");
        assert!(a(&["--cog", "lookout", "--ticket", "MADY-1"]).is_err(), "lookout takes no key");
        assert!(a(&["--cog", "worker", "--convoy", "MADY-100"]).is_err(), "worker can't use --convoy");
        assert!(a(&["--cog", "foreman", "--ticket", "MADY-1"]).is_err(), "unknown cog");
    }

    #[test]
    fn render_plan_notes_an_empty_plan() {
        assert!(render_plan(&tick(serde_json::json!({ "dry_run": true, "planned": [] })))
            .contains("nothing to do"));
    }

    #[test]
    fn render_plan_marks_live_applied_and_failed() {
        let out = render_plan(&tick(serde_json::json!({
            "dry_run": false,
            "applied": [ { "action": "reap", "ticket": "MADY-2" } ],
            "failed": [ { "action": { "action": "spawn", "ticket": "MADY-3" }, "error": "no worktree" } ]
        })));
        assert!(out.contains("(live)"));
        assert!(out.contains("✓") && out.contains("reap") && out.contains("MADY-2"));
        assert!(out.contains("✗") && out.contains("MADY-3") && out.contains("no worktree"));
    }

    fn sess(kind: CrewKind, ticket: Option<&str>, convoy: Option<&str>, worktree: &str, hid: Option<&str>) -> domain::Session {
        domain::Session {
            id: domain::SessionId(1),
            kind,
            ticket: ticket.map(|t| domain::TicketKey(t.into())),
            convoy: convoy.map(|c| domain::ConvoyKey(c.into())),
            worktree_path: worktree.into(),
            tmux_session: "x".into(),
            harness_session_id: hid.map(Into::into),
            created_at: "t".into(),
            ended_at: None,
            anchored_at: None,
        }
    }

    #[test]
    fn transcript_path_uses_the_stored_session_id() {
        // A non-existent worktree → canonicalize falls back to the raw path, encoded `/`→`-`.
        let s = sess(CrewKind::Worker, Some("MADY-1"), None, "/wt/MADY-1", Some("abc-123"));
        let got = transcript_path(&s).unwrap();
        let want = format!("{}/.claude/projects/-wt-MADY-1/abc-123.jsonl", std::env::var("HOME").unwrap());
        assert_eq!(got.to_string_lossy(), want);
    }

    #[test]
    fn matches_target_keys_on_ticket_convoy_or_singleton() {
        let ct = |cog: &str, ticket: Option<&str>, convoy: Option<&str>| wire::CogTarget {
            cog: cog.into(),
            ticket: ticket.map(Into::into),
            convoy: convoy.map(Into::into),
        };
        assert!(matches_target(&sess(CrewKind::Worker, Some("MADY-1"), None, "/wt", None), &ct("worker", Some("MADY-1"), None)));
        assert!(!matches_target(&sess(CrewKind::Worker, Some("MADY-2"), None, "/wt", None), &ct("worker", Some("MADY-1"), None)));
        assert!(matches_target(&sess(CrewKind::Captain, None, Some("MADY-100"), "/hub", None), &ct("captain", None, Some("MADY-100"))));
        assert!(matches_target(&sess(CrewKind::Lookout, None, None, "/hub", None), &ct("lookout", None, None)));
    }

    #[test]
    fn parse_send_takes_the_target_flags_and_the_message_positional() {
        let a = |args: &[&str]| parse_send(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        let p = a(&["--cog", "worker", "--ticket", "MADY-1", "handle the edge case"]).unwrap();
        assert_eq!(
            (p.cog.as_str(), p.ticket.as_deref(), p.convoy, p.message.as_str()),
            ("worker", Some("MADY-1"), None, "handle the edge case")
        );
        // lookout takes the message, no key.
        assert_eq!(a(&["--cog", "lookout", "look again"]).unwrap().message, "look again");
        // errors: missing message, bad target shape, and an unquoted (second) positional.
        assert!(a(&["--cog", "lookout"]).is_err(), "needs a message");
        assert!(a(&["--cog", "worker", "hi"]).is_err(), "worker needs --ticket");
        assert!(a(&["--cog", "lookout", "two", "words"]).is_err(), "unquoted message → extra arg");
    }

    #[test]
    fn render_settings_tabulates_lever_value_and_source() {
        let out = render_settings(
            r#"[{"key":"dry_run","value":"true","source":"setting"},{"key":"poll_secs","value":"5","source":"config"}]"#,
        )
        .unwrap();
        assert!(out.contains("LEVER") && out.contains("VALUE") && out.contains("SOURCE"));
        assert!(out.contains("dry_run") && out.contains("true") && out.contains("setting"));
        assert!(out.contains("poll_secs") && out.contains("config"));
    }
}
