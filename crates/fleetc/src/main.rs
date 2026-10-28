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

/// The fleet project this invocation talks to — the one enclosing the cwd, or `-C <dir>`'s.
/// Resolved once in `run` (before any verb), so every socket + ledger read agrees.
static PROJECT: std::sync::OnceLock<wire::Project> = std::sync::OnceLock::new();

fn project_dir() -> &'static wire::Project {
    PROJECT.get().expect("the project is resolved before any verb runs")
}

/// The project's fleetd socket.
fn socket() -> String {
    project_dir().socket()
}

/// fleetc's usage: a header, then one line per verb form (`fleetc <verb> …`).
const USAGE: &str = "usage: fleetc [-C <project-dir>] <verb> …   (the project: the cwd's, or -C's — nearest folder with a fleet.toml; in a cog, an omitted ticket / --iid / --convoy is its own)\n  \
     fleetc status   --cog <worker|sailor|hand|lookout> [--ticket <KEY-N>] --state <state> [--last-action <text>]\n  \
     fleetc dispatch --ticket <KEY-N> --ac <summary> [--hand] [--fork-from <ref>] [--worktree <path>] [--park-at <where>]\n  \
     fleetc hand     [--worktree <path>]   (ticketless operator-driven hand in a worktree; defaults to cwd)\n  \
     fleetc convoy   open <KEY> [--title <text>] | close [<KEY>]\n  \
     fleetc convoy   <add|remove> <KEY> <KEY-N>   (a ticket joins the convoy — its only one — or an added one leaves)\n  \
     fleetc anchor   <KEY-N> [--reason <why>]   (hold everything on it: off the frontier, its cogs stopped, its MR disarmed + drafted)\n  \
     fleetc anchor   --cog <captain|lookout> [--convoy <KEY>]   (stop a ticketless cog)\n  \
     fleetc weigh    <KEY-N> | --cog <captain|lookout> [--convoy <KEY>]   (release what anchor held)\n  \
     fleetc sink     --cog <kind> [--ticket <KEY-N>|--convoy <KEY>]   (scuttle — hard teardown)\n  \
     fleetc send     --cog <kind> [--ticket <KEY-N>|--convoy <KEY>] <message>   (message into the cog's pane)\n  \
     fleetc decommission <KEY-N> --reason <why> [--yes]   (withdraw it for good: close its MR, cancel it with the reason, reap its cogs + worktree)\n  \
     fleetc packet   [<KEY-N>]        (the cog's work-packet: order + ship + ticket)\n  \
     fleetc context  [--convoy <KEY>] [--format table]   (a captain's convoy slice)\n  \
     fleetc plan                       (fleetd's latest reconcile plan — the parity lens)\n  \
     fleetc transcript --cog <kind> [--ticket <KEY-N>|--convoy <KEY>] [--open]   (resolve a cog's transcript)\n  \
     fleetc review   [--iid <N>]       (an MR's fresh review surface: open threads + floating notes)\n  \
     fleetc reply    [--iid <N>] --thread <id> --body <text|-> [--resolve]   (answer a review thread)\n  \
     fleetc ship     [--iid <N>]       (one MR, ours or an ally's: ship + snapshot + fetch ref, as JSON)\n  \
     fleetc review-post [--iid <N>] --body <text|-> [--approve]   (post one review comment; approve if clean)\n  \
     fleetc rebase   [--iid <N>]       (server-side rebase an MR)\n  \
     fleetc ci       [--iid <N>]       (the MR's CI: status + each failed job's log excerpt, as JSON)\n  \
     fleetc retry-ci [--iid <N>]       (re-run the MR's failed CI jobs)\n  \
     fleetc arm      [<KEY-N>]        (arm the ticket's MR to merge once green; refused while held or blocked)\n  \
     fleetc ticket   [<KEY-N>] [--where]    (a ticket's full text; --where: its URL or file path)\n  \
     fleetc ticket   new --title <t> --type <story|bug|task|epic> [--convoy <KEY>] [--blocked-by <KEY-N>]… [--body <text>|-]   (prints the new key)\n  \
     fleetc ticket   block <KEY-N> --by <KEY-N>   (record an 'is blocked by' edge)\n  \
     fleetc ticket   edit [<KEY-N>] --body <text|->   (replace the description, whole: read it first)\n  \
     fleetc ticket   comment [<KEY-N>] --body <text|->   (comment on the ticket)\n  \
     fleetc propose  --title <t> --type <type> [--convoy <KEY>] [--blocked-by <KEY-N>]… [--body <text>|-]   (a cog: file a new ticket labelled fleet-proposed; prints its id + key)\n  \
     fleetc propose  --existing <KEY-N> --why <text> [--convoy <KEY>]   (a cog: propose taking on an existing ticket)\n  \
     fleetc proposal [<id> | accept <id> [--convoy <KEY>] | drop <id>]   (the open proposals; one in full; accept — official, into --convoy; drop — a proposed ticket is cancelled)\n  \
     fleetc launch   --title <t> --body <text> [--ticket <KEY-N>] [--target <branch>] [--draft] [--worktree <path>]   (open the MR for the worktree's branch; prints its URL)\n  \
     fleetc settings [list | set <key> <value>]   (live config levers; no restart)\n  \
     fleetc project                   (the project checkout's path, from fleet.toml)\n  \
     fleetc cost     [--since <RFC3339>]\n  \
     fleetc ledger   [<sql> | --full]  (read the project's ledger: one query as a table, every table, or a sqlite3 shell; read-only)\n  \
     fleetc export   <file>            (snapshot the project's ledger into <file>; safe while fleetd runs)\n  \
     fleetc import   <file>            (restore a ledger snapshot; fleetd must be stopped)";

/// One verb's usage — every form of it, under the header — for `fleetc <verb> --help`.
fn verb_usage(verb: &str) -> Option<String> {
    let mut lines = USAGE.lines();
    let header = lines.next()?;
    let forms: Vec<&str> = lines.filter(|l| l.split_whitespace().nth(1) == Some(verb)).collect();
    (!forms.is_empty()).then(|| format!("{header}\n{}", forms.join("\n")))
}

fn run() -> Result<()> {
    let (dir, args) = wire::split_dir_flag(std::env::args().skip(1).collect())?;
    // `--help` / `-h` anywhere after a verb: its usage (no project needed).
    if let Some(usage) = args.split_first().and_then(|(verb, rest)| {
        rest.iter().any(|a| a == "--help" || a == "-h").then(|| verb_usage(verb).unwrap_or_else(|| USAGE.into()))
    }) {
        println!("{usage}");
        return Ok(());
    }
    if !args.is_empty() {
        PROJECT.set(wire::Project::find(dir.as_deref())?).expect("set once");
    }
    match args.first().map(String::as_str) {
        Some("status") => status(&args[1..]),
        Some("dispatch") => dispatch(&args[1..]),
        Some("hand") => hand(&args[1..]),
        Some("convoy") => convoy(&args[1..]),
        Some("anchor") => anchor(&args[1..]),
        Some("weigh") => weigh(&args[1..]),
        Some("sink") => sink(&args[1..]),
        Some("send") => send(&args[1..]),
        Some("decommission") => decommission(&args[1..]),
        Some("packet") => packet(&args[1..]),
        Some("context") => context(&args[1..]),
        Some("plan") => plan(&args[1..]),
        Some("transcript") => transcript(&args[1..]),
        Some("review") => review(&args[1..]),
        Some("rebase") => rebase(&args[1..]),
        Some("ci") => ci(&args[1..]),
        Some("reply") => reply(&args[1..]),
        Some("ship") => ship(&args[1..]),
        Some("export") => export(&args[1..]),
        Some("ledger") => read_ledger(&args[1..]),
        Some("import") => import(&args[1..]),
        Some("review-post") => review_post(&args[1..]),
        Some("retry-ci") => retry_ci(&args[1..]),
        Some("launch") => launch(&args[1..]),
        Some("ticket") => ticket(&args[1..]),
        Some("propose") => propose(&args[1..]),
        Some("proposal") => proposal(&args[1..]),
        Some("arm") => arm(&args[1..]),
        Some("settings") => settings(&args[1..]),
        Some("project") => project(),
        Some("cost") => cost(&args[1..]),
        _ => {
            eprintln!("{USAGE}");
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
    let state = state.ok_or_else(|| anyhow!("--state required"))?;
    let kind = CrewKind::parse(&cog).ok_or_else(|| anyhow!("invalid --cog: {cog}"))?;
    // The lookout is a singleton: fleetd finds it without a ticket.
    let ticket = match (ticket.or_else(|| own("FLEET_TICKET")), kind) {
        (Some(t), _) => Some(ticket_key(&t, "status")?),
        (None, CrewKind::Lookout) => None,
        (None, _) => bail!("--ticket required"),
    };
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
/// reconcile to spawn. Defaults: a worker.
fn dispatch(args: &[String]) -> Result<()> {
    let (mut ticket, mut ac, mut fork_from, mut park_at, mut worktree) = (None, None, None, None, None);
    let mut auto_impl = true;
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
        fork_from,
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
/// reaps the captain next tick). `fleetc convoy add|remove <KEY> <KEY-N>` — a ticket joins the
/// convoy beyond its epic's children (one convoy per ticket), or an added one leaves.
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
            let key = convoy_key(&key.ok_or_else(|| anyhow!("convoy open needs <KEY>"))?, "convoy open")?;
            submit("/convoy-open", &wire::ConvoyCmd { key, title })
        }
        Some("close") => {
            let key = args.get(1).cloned().or_else(|| own("FLEET_CONVOY")).ok_or_else(|| anyhow!("convoy close needs <KEY>"))?;
            let key = convoy_key(&key, "convoy close")?;
            submit("/convoy-close", &wire::ConvoyCmd { key, title: None })
        }
        Some(verb @ ("add" | "remove")) => {
            let [convoy, ticket] = &args[1..] else { bail!("usage: fleetc convoy {verb} <KEY> <KEY-N>") };
            let what = format!("convoy {verb}");
            let cmd = wire::ConvoyMemberCmd { convoy: convoy_key(convoy, &what)?, ticket: ticket_key(ticket, &what)? };
            post_and_print(&format!("/convoy-{verb}"), &cmd)
        }
        _ => bail!("usage: fleetc convoy <open|close> <KEY> [--title <text>] | <add|remove> <KEY> <KEY-N>"),
    }
}

/// `fleetc anchor <KEY-N> [--reason <why>]` — hold everything on the ticket, whatever its stage: off
/// the frontier, its live cogs stopped (they checkpoint), its open MR disarmed + drafted. `--cog
/// captain --convoy <KEY>` / `--cog lookout` anchor a ticketless cog. To tear a cog down, `sink`.
fn anchor(args: &[String]) -> Result<()> {
    match args.first() {
        Some(key) if !key.starts_with('-') => {
            let reason = match &args[1..] {
                [] => None,
                [flag, why] if flag == "--reason" => Some(why.clone()),
                _ => bail!("usage: fleetc anchor <KEY-N> [--reason <why>]"),
            };
            submit("/anchor-ticket", &wire::TicketAnchor { ticket: ticket_key(key, "anchor")?, reason })
        }
        _ => submit("/anchor", &ticketless_target(args, "anchor")?),
    }
}

/// `fleetc weigh <KEY-N>` — weigh anchor (get under way): release all `anchor` held. `--cog captain
/// --convoy <KEY>` / `--cog lookout` for a ticketless cog.
fn weigh(args: &[String]) -> Result<()> {
    match args {
        [key] if !key.starts_with('-') => {
            submit("/weigh-ticket", &wire::TicketAnchor { ticket: ticket_key(key, "weigh")?, reason: None })
        }
        _ => submit("/weigh", &ticketless_target(args, "weigh")?),
    }
}

/// A `--cog` target for anchor/weigh: a ticketed cog is anchored through its ticket, never alone.
fn ticketless_target(args: &[String], verb: &str) -> Result<wire::CogTarget> {
    let target = require_cog_target(args, verb)?;
    if target.ticket.is_some() {
        bail!("{verb} the ticket, which holds its cog too: fleetc {verb} <KEY-N>");
    }
    Ok(target)
}

/// `fleetc sink --cog <kind> [--ticket|--convoy]` — scuttle the cog: a deliberate, destructive
/// teardown (end the session + kill the tmux). Its own verb — sinking a ship is not casual.
fn sink(args: &[String]) -> Result<()> {
    submit("/sink", &require_cog_target(args, "sink")?)
}

/// `fleetc decommission <KEY-N> --reason <why> [--yes]` — withdraw a ticket from the fleet for good:
/// fleetd closes its open MR (noting why), comments the reason on the ticket and cancels it, then
/// reaps its cogs, worktree and order. The operator's call — a cog can't — and it asks first, showing
/// what will go, unless `--yes`. fleetd refuses once its MR has merged.
fn decommission(args: &[String]) -> Result<()> {
    if std::env::var("FLEET_TICKET").is_ok_and(|t| !t.is_empty()) {
        bail!("a cog doesn't decommission tickets — the operator does (propose it instead)");
    }
    let usage = "usage: fleetc decommission <KEY-N> --reason <why> [--yes]";
    let ticket = ticket_key(args.first().ok_or_else(|| anyhow!("{usage}"))?, "decommission")?;
    let (mut reason, mut yes) = (None, false);
    let mut it = args[1..].iter();
    while let Some(flag) = it.next() {
        match flag.as_str() {
            "--reason" => reason = Some(it.next().cloned().ok_or_else(|| anyhow!("--reason needs a value"))?),
            "--yes" => yes = true,
            other => bail!("unknown flag {other:?} — {usage}"),
        }
    }
    let reason = reason.filter(|r| !r.trim().is_empty()).ok_or_else(|| anyhow!("decommission needs --reason <why> — it goes on the ticket"))?;
    if !yes {
        eprint!("{}Proceed? [y/N] ", decommission_preview(&ticket)?);
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer)?;
        if !matches!(answer.trim(), "y" | "Y" | "yes") {
            bail!("not decommissioned");
        }
    }
    post_and_print("/decommission", &wire::DecommissionCmd { ticket, reason })
}

/// What a decommission takes down, read off the ledger: its open MRs, its live cogs, its worktree.
fn decommission_preview(ticket: &str) -> Result<String> {
    use domain::ports::{OrderRepo, SessionRepo, ShipRepo};
    let db = project_dir().ledger();
    let ledger = ledger::Ledger::open_readonly(&db).map_err(|_| anyhow!("no ledger yet ({db})"))?;
    let key = domain::TicketKey(ticket.to_string());
    let mut out = format!("decommission {ticket} — for good:\n");
    for ship in ShipRepo::armada(&ledger)?.into_iter().filter(|s| s.ticket.as_ref() == Some(&key)) {
        out += &format!("  close !{} (disarmed first, the reason noted on it)\n", ship.iid);
    }
    out += &format!("  comment the reason on {ticket}, then move it to Cancelled\n");
    for s in SessionRepo::for_ticket(&ledger, &key)?.into_iter().filter(|s| s.ended_at.is_none()) {
        out += &format!("  end {} (its tmux too)\n", s.tmux_session);
    }
    if let Some(wt) = OrderRepo::get(&ledger, &key)?.and_then(|o| o.worktree_path) {
        out += &format!("  remove the worktree {wt} and its local branch, retire the order\n");
    }
    Ok(out)
}

/// `fleetc packet <KEY-N>` — read the cog's composed work-packet (order + ship + snapshot +
/// ticket) from fleetd and print it as JSON. The sailor's boot read; also handy for an operator.
fn packet(args: &[String]) -> Result<()> {
    let ticket = ticket_or_own(args, "packet")?;
    let socket = socket();
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
    let convoy = convoy_key(&convoy.or_else(|| own("FLEET_CONVOY")).ok_or_else(|| anyhow!("context needs --convoy <KEY>"))?, "context")?;
    let socket = socket();
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
    let waiting = |t: &serde_json::Value| -> String {
        let keys: Vec<&str> = t["waiting_on"].as_array().into_iter().flatten().filter_map(|k| k.as_str()).collect();
        match (keys.is_empty(), t["held"].as_str()) {
            (_, Some(reason)) => format!("held{}", if reason.is_empty() { String::new() } else { format!(": {reason}") }),
            (false, None) => format!("blocked by {}", keys.join(", ")),
            (true, None) => String::new(),
        }
    };
    let mut out = format!("{:<12} {:<12} {:<8} {:<10} {:<8} {:<8} {}\n", "TICKET", "STATUS", "CLASS", "DISPATCH", "COG", "SHIP", "WAITING");
    for t in tickets {
        out += &format!(
            "{:<12} {:<12} {:<8} {:<10} {:<8} {:<8} {}\n",
            cell(t, "key"),
            cell(t, "status"),
            cell(t, "classification"),
            cell(t, "dispatched"),
            cell(t, "live_cog"),
            cell(t, "ship_state"),
            waiting(t),
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

/// A fleetd-injected cog env var (`FLEET_TICKET`, `FLEET_CONVOY`, `FLEET_SHIP`): what a cog's verbs
/// fill an omitted key with, so its commands carry no shell variable (Claude Code's allow rules
/// match only a command it can check before it runs; one with `"$FLEET_TICKET"` goes to auto mode's
/// classifier, which may block it).
fn own(var: &str) -> Option<String> {
    std::env::var(var).ok().filter(|v| !v.is_empty())
}

/// A ticket verb's `<KEY-N>`: the first arg, else the cog's own ticket.
fn ticket_or_own(args: &[String], verb: &str) -> Result<String> {
    match (args.is_empty(), own("FLEET_TICKET")) {
        (true, Some(t)) => ticket_key(&t, verb),
        _ => require_ticket(args, verb),
    }
}

/// An MR verb's iid: `--iid` when given, else the cog's own MR — the lookout's `FLEET_SHIP`, or the
/// open MR of its `FLEET_TICKET`.
fn iid_or_own(iid: Option<i64>, verb: &str) -> Result<i64> {
    if let Some(iid) = iid {
        return Ok(iid);
    }
    if let Some(s) = own("FLEET_SHIP") {
        return s.parse().map_err(|_| anyhow!("{verb}: FLEET_SHIP isn't an iid: {s:?}"));
    }
    let ticket = own("FLEET_TICKET").ok_or_else(|| anyhow!("{verb} needs --iid <N>"))?;
    let (code, body) = wire::get_json(&socket(), &format!("/packet?ticket={ticket}"))?;
    anyhow::ensure!(code == 200, "fleetd ({code}): {body}");
    serde_json::from_str::<serde_json::Value>(&body)?
        .pointer("/ship/iid")
        .and_then(|i| i.as_i64())
        .ok_or_else(|| anyhow!("{verb}: {ticket} has no open MR"))
}

/// Validate a ticket key at the edge — the domain's `<PREFIX>-<n>` parse, tagged with the verb.
fn ticket_key(t: &str, verb: &str) -> Result<String> {
    domain::TicketKey::parse(t).map(|k| k.0).map_err(|e| anyhow!("{verb}: {e}"))
}

/// Validate a convoy key at the edge — same shape as a ticket key.
fn convoy_key(c: &str, verb: &str) -> Result<String> {
    domain::ConvoyKey::parse(c).map(|k| k.0).map_err(|e| anyhow!("{verb}: {e}"))
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
            let c = convoy.ok_or_else(|| anyhow!("{verb} --cog captain needs --convoy <KEY>"))?;
            convoy_key(c, verb)?;
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
    let socket = socket();
    let (code, resp) = wire::post_json(&socket, path, body)?;
    if code == 200 {
        Ok(())
    } else {
        bail!("fleetd rejected the request ({code}): {resp}")
    }
}

/// `fleetc plan` — show fleetd's latest reconcile plan: what it *would* do (dry-run: `planned[]`)
/// or *did* (live: `applied[]`/`failed[]`) last tick. The parity lens — read-only, so it works
/// while fleetd writes. Reads the project's `.fleet/ledger.db`.
fn plan(args: &[String]) -> Result<()> {
    if let Some(flag) = args.first() {
        bail!("unknown flag: {flag}");
    }
    let db = project_dir().ledger();
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

    let db = project_dir().ledger();
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

/// Map a live session to its transcript jsonl: the stored harness id's file, or — no stored id —
/// the newest jsonl in the session's transcript folder.
fn transcript_path(s: &domain::Session) -> Result<std::path::PathBuf> {
    if let Some(path) = s.transcript() {
        return Ok(path);
    }
    let project_dir = s.transcript_dir().ok_or_else(|| anyhow!("HOME unset"))?;
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
    let socket = socket();
    let (code, body) = wire::get_json(&socket, &format!("/review?iid={iid}"))?;
    if code != 200 {
        bail!("fleetd ({code}): {body}");
    }
    println!("{body}");
    Ok(())
}

/// `fleetc reply --iid <N> --thread <id> --body <text|-> [--resolve]` — answer one review thread
/// (`--body -` reads stdin), resolving it once its ask is addressed.
fn reply(args: &[String]) -> Result<()> {
    let (mut iid, mut thread, mut body, mut resolve) = (None, None, None, false);
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        let mut take = || it.next().cloned().ok_or_else(|| anyhow!("{flag} needs a value"));
        match flag.as_str() {
            "--iid" => iid = Some(take()?.parse::<i64>().map_err(|_| anyhow!("--iid must be a number"))?),
            "--thread" => thread = Some(take()?),
            "--body" => body = Some(take()?),
            "--resolve" => resolve = true,
            other => bail!("unknown flag: {other}"),
        }
    }
    let body = body_text(body.ok_or_else(|| anyhow!("--body required"))?)?;
    post_and_print("/reply", &wire::ReplyCmd {
        iid: iid_or_own(iid, "reply")?,
        thread: thread.ok_or_else(|| anyhow!("--thread required"))?,
        body,
        resolve,
    })
}

/// `fleetc export <file>` — snapshot the project's ledger (its state: tickets, ships, orders,
/// sessions, events) into `<file>`: a consistent, standalone copy (`VACUUM INTO`), taken read-only
/// beside a running fleetd. Name it as you like, e.g. with `$(date -u +%Y%m%dT%H%M%SZ)`.
/// Telemetry (the spend log) isn't state and isn't exported.
/// `fleetc ledger [<sql> | --full]` — read the project's ledger through `sqlite3 -readonly -box`:
/// the query printed as a table; `--full`, every table (the event log: its newest rows); with
/// neither, an interactive shell (`.tables`, `.schema <t>`). Read-only: fleetd stays the single
/// writer. Exits with sqlite3's own status.
fn read_ledger(args: &[String]) -> Result<()> {
    let ledger = project_dir().ledger();
    anyhow::ensure!(std::path::Path::new(&ledger).exists(), "no ledger yet: {ledger} doesn't exist (start fleetd once)");
    let sqlite = || {
        let mut c = std::process::Command::new("sqlite3");
        c.args(["-readonly", "-box", &ledger]);
        c
    };
    let spawned = |e: std::io::Error| match e.kind() {
        std::io::ErrorKind::NotFound => anyhow!("sqlite3 isn't on PATH (needs 3.33+ for -box)"),
        _ => anyhow!("sqlite3: {e}"),
    };
    let status = match args {
        [flag] if flag == "--full" => {
            let names = sqlite().arg("-list").arg(TABLES).output().map_err(spawned)?;
            anyhow::ensure!(names.status.success(), "sqlite3: {}", String::from_utf8_lossy(&names.stderr).trim());
            let script = full_dump(&String::from_utf8_lossy(&names.stdout));
            let mut child = sqlite().stdin(std::process::Stdio::piped()).spawn().map_err(spawned)?;
            use std::io::Write as _;
            child.stdin.take().expect("piped stdin").write_all(script.as_bytes())?;
            child.wait()?
        }
        [] => sqlite().status().map_err(spawned)?,
        _ => sqlite().arg(args.join(" ")).status().map_err(spawned)?,
    };
    std::process::exit(status.code().unwrap_or(1));
}

/// The ledger's tables, by name (sqlite's own bookkeeping left out).
const TABLES: &str = "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name";
/// How much of the event log `--full` shows — it's thousands of rows; the rest is one query away.
const EVENTS_SHOWN: usize = 50;

/// The sqlite3 script `ledger --full` runs: each table under a `── name · N rows` heading, long
/// cells wrapped; the event log newest first, cut to `EVENTS_SHOWN`.
fn full_dump(tables: &str) -> String {
    let mut script = String::new();
    for t in tables.lines().map(str::trim).filter(|t| !t.is_empty()) {
        let (rows, note) = if t == "events" {
            (format!("SELECT * FROM events ORDER BY seq DESC LIMIT {EVENTS_SHOWN};"), format!(" (newest {EVENTS_SHOWN})"))
        } else {
            (format!("SELECT * FROM \"{t}\";"), String::new())
        };
        script.push_str(&format!(
            ".mode list\nSELECT char(10) || '── {t} · ' || count(*) || ' rows{note}' FROM \"{t}\";\n.mode box --wrap 60\n{rows}\n"
        ));
    }
    script
}

fn export(args: &[String]) -> Result<()> {
    let [file] = args else { bail!("usage: fleetc export <file>") };
    let ledger = project_dir().ledger();
    anyhow::ensure!(std::path::Path::new(&ledger).exists(), "no state to export: {ledger} doesn't exist yet");
    anyhow::ensure!(!std::path::Path::new(file).exists(), "{file} already exists — export to a new file");
    if let Some(dir) = std::path::Path::new(file).parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    ledger::snapshot(&ledger, file)?;
    println!("exported the ledger → {file}");
    Ok(())
}

/// `fleetc import <file>` — restore a ledger snapshot into the project's `.fleet/`. Refused while
/// fleetd is running for this project, or when the file's schema version isn't this build's.
fn import(args: &[String]) -> Result<()> {
    let [file] = args else { bail!("usage: fleetc import <file>") };
    import_into(project_dir(), std::path::Path::new(file))
}

fn import_into(project: &wire::Project, file: &std::path::Path) -> Result<()> {
    // 1. fleetd must not be running: something answering on the project's socket means it is. A
    //    socket file nothing answers on is a crash leftover — remove it.
    let socket = project.socket();
    match std::os::unix::net::UnixStream::connect(&socket) {
        Ok(_) => bail!("fleetd is running for this project — stop it first (make stop), then import"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => std::fs::remove_file(&socket)?,
        Err(e) => bail!("can't tell whether fleetd is running ({socket}: {e})"),
    }
    // 2. Check the snapshot before touching anything.
    let found = ledger::schema_version(&file.to_string_lossy())
        .map_err(|e| anyhow!("{}: not a ledger snapshot ({e})", file.display()))?;
    anyhow::ensure!(
        found == ledger::SCHEMA_VERSION,
        "{} is ledger schema v{found}, this build reads v{} — use a matching build",
        file.display(),
        ledger::SCHEMA_VERSION
    );
    // 3. Restore. The live ledger's -wal/-shm go first: a stale write-ahead log would be replayed
    //    onto the restored database. The copy lands under a temp name, then renames into place.
    std::fs::create_dir_all(project.runtime())?;
    let dst = project.ledger();
    for side in ["-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{dst}{side}"));
    }
    let tmp = format!("{dst}.importing");
    std::fs::copy(file, &tmp)?;
    std::fs::rename(&tmp, &dst)?;
    println!("imported {} → the ledger — start the fleet to resume from it", file.display());
    Ok(())
}

/// `fleetc ship --iid <N>` — one MR (ours or an ally's) as JSON: the ship, its snapshot, and the
/// ref to fetch its head by (`git fetch origin <fetch_ref>`).
fn ship(args: &[String]) -> Result<()> {
    let iid = require_iid(args, "ship")?;
    let (code, body) = wire::get_json(&socket(), &format!("/ship?iid={iid}"))?;
    if code != 200 {
        bail!("fleetd ({code}): {body}");
    }
    println!("{body}");
    Ok(())
}

/// `fleetc review-post --iid <N> --body <text|-> [--approve]` — post one review comment on an MR
/// (`--body -` reads stdin), then approve it when asked.
fn review_post(args: &[String]) -> Result<()> {
    let (mut iid, mut body, mut approve) = (None, None, false);
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        let mut take = || it.next().cloned().ok_or_else(|| anyhow!("{flag} needs a value"));
        match flag.as_str() {
            "--iid" => iid = Some(take()?.parse::<i64>().map_err(|_| anyhow!("--iid must be a number"))?),
            "--body" => body = Some(take()?),
            "--approve" => approve = true,
            other => bail!("unknown flag: {other}"),
        }
    }
    let body = body_text(body.ok_or_else(|| anyhow!("--body required"))?)?;
    post_and_print("/review-post", &wire::ReviewPostCmd { iid: iid_or_own(iid, "review-post")?, body, approve })
}

/// `fleetc ci --iid <N>` — the MR's CI as JSON: `{status, failed: [{name, url, log_excerpt}]}`, failed
/// child pipelines included — what fleet-repair classifies.
fn ci(args: &[String]) -> Result<()> {
    let iid = require_iid(args, "ci")?;
    let (code, body) = wire::get_json(&socket(), &format!("/ci?iid={iid}"))?;
    if code != 200 {
        bail!("fleetd ({code}): {body}");
    }
    println!("{body}");
    Ok(())
}

/// `fleetc retry-ci --iid <N>` — re-run the MR's failed CI jobs (a transient failure's fix).
fn retry_ci(args: &[String]) -> Result<()> {
    let iid = require_iid(args, "retry-ci")?;
    post_and_print(&format!("/retry-ci?iid={iid}"), &serde_json::json!({}))
}

/// `fleetc rebase --iid <N>` — server-side rebase the MR via the factory's forge (`GlabForge::rebase`).
fn rebase(args: &[String]) -> Result<()> {
    let iid = require_iid(args, "rebase")?;
    post_and_print(&format!("/rebase?iid={iid}"), &serde_json::json!({}))
}

/// `fleetc launch --title <t> --body <text> [--ticket <KEY-N>] [--target <branch>] [--draft]
/// [--worktree <path>]` — launch a ship: open the MR for the branch checked out in the worktree
/// (default: the current dir), assigned to you. The target defaults to the ticket's fork base,
/// else origin's default branch. The branch must already be pushed. Prints the MR's URL.
fn launch(args: &[String]) -> Result<()> {
    let (mut ticket, mut target, mut title, mut description, mut worktree) = (None, None, None, None, None);
    let mut draft = false;
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        let mut take = || it.next().cloned().ok_or_else(|| anyhow!("{flag} needs a value"));
        match flag.as_str() {
            "--ticket" => ticket = Some(ticket_key(&take()?, "launch")?),
            "--target" => target = Some(take()?),
            "--title" => title = Some(take()?),
            "--body" => description = Some(take()?),
            "--worktree" => worktree = Some(take()?),
            "--draft" => draft = true,
            other => bail!("unknown flag: {other}"),
        }
    }
    let ticket = match ticket {
        Some(t) => Some(t),
        None => own("FLEET_TICKET").map(|t| ticket_key(&t, "launch")).transpose()?,
    };
    let worktree = match worktree {
        Some(w) => w,
        None => std::env::current_dir()?.to_string_lossy().into_owned(),
    };
    post_and_print("/launch", &wire::LaunchCmd {
        ticket,
        worktree,
        target,
        title: title.ok_or_else(|| anyhow!("--title required"))?,
        description: description.ok_or_else(|| anyhow!("--body required"))?,
        draft,
    })
}

/// `fleetc arm <KEY-N>` — arm the ticket's open MR to merge once its gates pass (the sailor's land
/// step). fleetd refuses while the ticket is held or has an open blocker, and says which.
fn arm(args: &[String]) -> Result<()> {
    let ticket = ticket_or_own(args, "arm")?;
    post_and_print("/arm", &wire::ArmCmd { ticket })
}

/// `fleetc ticket <KEY-N> | new … | block … | edit … | comment …` — read or write tickets through
/// the project's tracker (Jira, the filesystem, …): one verb, whichever adapter fleet.toml picks.
fn ticket(args: &[String]) -> Result<()> {
    match args.first().map(String::as_str) {
        Some("new") => ticket_new(&args[1..]),
        Some("block") => ticket_block(&args[1..]),
        Some(sub @ ("edit" | "comment")) => ticket_text(sub, &args[1..]),
        Some(key) if !key.starts_with('-') => match args.get(1).map(String::as_str) {
            Some("--where") => ticket_where(&ticket_key(key, "ticket")?),
            _ => ticket_show(&ticket_key(key, "ticket")?),
        },
        None | Some("--where") if own("FLEET_TICKET").is_some() => {
            let key = ticket_or_own(&[], "ticket")?;
            if args.is_empty() { ticket_show(&key) } else { ticket_where(&key) }
        }
        _ => bail!("usage: fleetc ticket [<KEY-N>] | new --title <t> --type <type> … | block <KEY-N> --by <KEY-N> | edit|comment [<KEY-N>] --body <text|->"),
    }
}

/// `fleetc ticket <KEY> --where` — print where a person opens the ticket: a URL (Jira) or a file
/// path (the filesystem tracker), for `open "$(fleetc ticket KEY --where)"`.
fn ticket_where(key: &str) -> Result<()> {
    let (code, body) = wire::get_json(&socket(), &format!("/ticket?ticket={key}"))?;
    if code != 200 {
        bail!("fleetd ({code}): {body}");
    }
    let d: serde_json::Value = serde_json::from_str(&body)?;
    let location = d["location"].as_str().unwrap_or("");
    if location.is_empty() {
        bail!("{key}: the tracker reports no location for it");
    }
    println!("{location}");
    Ok(())
}

/// Print a ticket's full text: a facts line, the title, its links, then the description.
fn ticket_show(key: &str) -> Result<()> {
    let (code, body) = wire::get_json(&socket(), &format!("/ticket?ticket={key}"))?;
    if code != 200 {
        bail!("fleetd ({code}): {body}");
    }
    let d: serde_json::Value = serde_json::from_str(&body)?;
    print!("{}", render_ticket(&d));
    Ok(())
}

fn render_ticket(d: &serde_json::Value) -> String {
    let t = &d["ticket"];
    let s = |v: &serde_json::Value| v.as_str().unwrap_or("").to_string();
    let mut facts = vec![s(&t["key"]), s(&d["kind"]), s(&t["status"])];
    if let Some(a) = t["assignee"].as_str() {
        facts.push(format!("assignee {a}"));
    }
    let mut links = Vec::new();
    if let Some(c) = t["convoy"].as_str() {
        links.push(format!("convoy {c}"));
    }
    let blockers: Vec<String> = t["blocked_by"].as_array().into_iter().flatten().map(s).collect();
    if !blockers.is_empty() {
        links.push(format!("blocked by {}", blockers.join(", ")));
    }
    let mut out = format!("{}\n{}\n", facts.join(" · "), s(&t["title"]));
    if !links.is_empty() {
        out.push_str(&format!("{}\n", links.join(" · ")));
    }
    if let Some(reason) = d["held"].as_str() {
        out.push_str(&format!("held by the fleet{}\n", if reason.is_empty() { String::new() } else { format!(": {reason}") }));
    }
    let description = s(&d["description"]);
    if !description.trim().is_empty() {
        out.push_str(&format!("\n{}\n", description.trim_end()));
    }
    out
}

/// `fleetc ticket new --title <t> --type <type> [--convoy <KEY>] [--blocked-by <KEY-N>]… [--body <text>|-]`
/// — create a ticket in To Do; `--body -` reads the description from stdin. Prints the new key.
fn ticket_new(args: &[String]) -> Result<()> {
    // A cog (fleetd sets its FLEET_TICKET) never files: the operator decides what reaches the tracker.
    if std::env::var("FLEET_TICKET").is_ok_and(|t| !t.is_empty()) {
        bail!("a cog doesn't file tickets — propose it for the operator: fleetc propose … (/fleet-propose)");
    }
    post_and_print("/ticket-new", &draft_flags(args, "ticket new")?)
}

/// The ticket-draft flags `ticket new` and `propose` share: `--title`, `--type`, `--convoy`,
/// `--blocked-by`…, `--body <text>|-` (`-` reads stdin).
fn draft_flags(args: &[String], verb: &str) -> Result<wire::TicketNewCmd> {
    let (mut title, mut kind, mut convoy, mut body) = (None, None, None, String::new());
    let mut blocked_by = Vec::new();
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        let mut take = || it.next().cloned().ok_or_else(|| anyhow!("{flag} needs a value"));
        match flag.as_str() {
            "--title" => title = Some(take()?),
            "--type" => kind = Some(take()?),
            "--convoy" => convoy = Some(convoy_key(&take()?, verb)?),
            "--blocked-by" => blocked_by.push(ticket_key(&take()?, verb)?),
            "--body" => body = take()?,
            other => bail!("unknown flag: {other}"),
        }
    }
    Ok(wire::TicketNewCmd {
        title: title.ok_or_else(|| anyhow!("--title required"))?,
        kind: kind.ok_or_else(|| anyhow!("--type required (story, bug, task, epic, …)"))?,
        description: body_text(body)?,
        convoy,
        blocked_by,
    })
}

/// `fleetc propose …` — a cog holds work it found for the operator instead of filing it: a new
/// ticket (the `ticket new` flags), or `--existing <KEY> --why <text>` to take on one that exists.
/// The proposing ticket is the cog's own (`FLEET_TICKET`, which fleetd injects at spawn); either
/// shape lands in its convoy on accept unless `--convoy` names another.
fn propose(args: &[String]) -> Result<()> {
    let proposed = if args.iter().any(|a| a == "--existing") {
        let (mut ticket, mut why, mut convoy) = (None, None, None);
        let mut it = args.iter();
        while let Some(flag) = it.next() {
            let mut take = || it.next().cloned().ok_or_else(|| anyhow!("{flag} needs a value"));
            match flag.as_str() {
                "--existing" => ticket = Some(ticket_key(&take()?, "propose")?),
                "--why" => why = Some(take()?),
                "--convoy" => convoy = Some(convoy_key(&take()?, "propose")?),
                other => bail!("propose --existing takes --why and --convoy, not {other}"),
            }
        }
        let why = why.ok_or_else(|| anyhow!("propose --existing needs --why <text>: why take it on"))?;
        let ticket = ticket.ok_or_else(|| anyhow!("propose --existing needs a <KEY-N>"))?;
        wire::ProposedCmd::Existing { ticket, why, convoy }
    } else {
        wire::ProposedCmd::New(draft_flags(args, "propose")?)
    };
    let from_ticket = match std::env::var("FLEET_TICKET") {
        Ok(t) if !t.is_empty() => Some(ticket_key(&t, "propose (FLEET_TICKET)")?),
        _ => None,
    };
    let (code, resp) = wire::post_json(&socket(), "/propose", &wire::ProposeCmd { from_ticket, proposed })?;
    if code != 200 {
        bail!("fleetd ({code}): {resp}");
    }
    println!("{resp}");
    Ok(())
}

/// `fleetc proposal` — the open proposals; `<id>`: one in full; `accept <id> [--convoy <KEY>]`:
/// accept it (a new ticket is filed — prints the ticket either way); `drop <id>`: discard it.
/// Reads are off the ledger.
fn proposal(args: &[String]) -> Result<()> {
    let id = |s: Option<&String>| -> Result<i64> {
        let s = s.ok_or_else(|| anyhow!("needs a proposal <id>"))?;
        s.parse().map_err(|_| anyhow!("a proposal id is a number, got {s:?}"))
    };
    match args.first().map(String::as_str) {
        None => print!("{}", render_proposals(&pending_proposals()?)),
        Some("accept") => {
            let convoy = match args.get(2..).unwrap_or_default() {
                [] => None,
                [flag, c] if flag == "--convoy" => Some(convoy_key(c, "proposal accept")?),
                _ => bail!("usage: fleetc proposal accept <id> [--convoy <KEY>]"),
            };
            post_and_print("/proposal-accept", &wire::ProposalCmd { id: id(args.get(1))?, convoy })?;
        }
        Some("drop") => post_and_print("/proposal-drop", &wire::ProposalCmd { id: id(args.get(1))?, convoy: None })?,
        Some(_) => {
            let want = id(args.first())?;
            let p = pending_proposals()?.into_iter().find(|p| p.id.0 == want).ok_or_else(|| anyhow!("no open proposal {want}"))?;
            print!("{}", render_proposal(&p));
        }
    }
    Ok(())
}

fn pending_proposals() -> Result<Vec<domain::Proposal>> {
    let db = project_dir().ledger();
    let ledger = ledger::Ledger::open_readonly(&db).map_err(|_| anyhow!("no ledger yet ({db})"))?;
    domain::ports::ProposalRepo::pending(&ledger)
}

fn render_proposals(ps: &[domain::Proposal]) -> String {
    if ps.is_empty() {
        return "no open proposals\n".into();
    }
    let key = |k: Option<&String>| k.cloned().unwrap_or_else(|| "—".into());
    ps.iter()
        .map(|p| {
            let from = key(p.from_ticket.as_ref().map(|k| &k.0));
            let convoy = key(p.proposed.convoy().map(|c| &c.0));
            let (kind, what) = match &p.proposed {
                domain::Proposed::New(d) => (d.kind.as_str(), d.title.clone()),
                domain::Proposed::Existing { ticket, why, .. } => ("exists", format!("{} · {why}", ticket.0)),
            };
            format!("{:<4} {from:<12} {convoy:<12} {kind:<6} {what}\n", p.id.0)
        })
        .collect()
}

fn render_proposal(p: &domain::Proposal) -> String {
    let mut facts = vec![format!("proposal {}", p.id.0)];
    let body = match &p.proposed {
        domain::Proposed::New(d) => {
            facts.extend([d.kind.clone(), p.created_at.clone()]);
            p.from_ticket.iter().for_each(|t| facts.push(format!("from {}", t.0)));
            d.convoy.iter().for_each(|c| facts.push(format!("convoy {}", c.0)));
            if !d.blocked_by.is_empty() {
                facts.push(format!("blocked by {}", d.blocked_by.iter().map(|k| k.0.as_str()).collect::<Vec<_>>().join(", ")));
            }
            format!("{}\n\n{}", d.title, d.description.trim_end())
        }
        domain::Proposed::Existing { ticket, why, convoy } => {
            facts.extend([format!("existing {}", ticket.0), p.created_at.clone()]);
            p.from_ticket.iter().for_each(|t| facts.push(format!("from {}", t.0)));
            convoy.iter().for_each(|c| facts.push(format!("convoy {}", c.0)));
            format!("{why}\n\n(the ticket itself: fleetc ticket {})", ticket.0)
        }
    };
    format!("{}\n{body}\n", facts.join(" · "))
}

/// `fleetc ticket edit|comment [<KEY-N>] --body <text|->` — replace the ticket's description
/// (`edit`: it goes whole, so read it first) or comment on it (`comment`).
fn ticket_text(sub: &str, args: &[String]) -> Result<()> {
    let verb = format!("ticket {sub}");
    let (key, flags) = match args.split_first() {
        Some((k, rest)) if !k.starts_with('-') => (ticket_key(k, &verb)?, rest),
        _ => (ticket_or_own(&[], &verb)?, args),
    };
    let body = match flags {
        [flag, body] if flag == "--body" => body_text(body.clone())?,
        _ => bail!("usage: fleetc ticket {sub} [<KEY-N>] --body <text|->"),
    };
    submit(&format!("/ticket-{sub}"), &wire::TicketTextCmd { ticket: key, body })
}

/// `fleetc ticket block <KEY-N> --by <KEY-N>` — record that the first ticket is blocked by the second.
fn ticket_block(args: &[String]) -> Result<()> {
    let key = ticket_key(args.first().ok_or_else(|| anyhow!("ticket block needs a <KEY-N>"))?, "ticket block")?;
    let by = match &args[1..] {
        [flag, by] if flag == "--by" => ticket_key(by, "ticket block")?,
        _ => bail!("usage: fleetc ticket block <KEY-N> --by <KEY-N>"),
    };
    submit("/ticket-block", &wire::TicketBlockCmd { ticket: key, by })
}

/// A `--body` value: as given, or stdin's whole text for `-`.
fn body_text(body: String) -> Result<String> {
    if body != "-" {
        return Ok(body);
    }
    let mut text = String::new();
    std::io::Read::read_to_string(&mut std::io::stdin(), &mut text)?;
    Ok(text)
}

/// POST `body` to fleetd and print its reply body (an action whose reply is the result).
fn post_and_print<T: serde::Serialize>(path: &str, body: &T) -> Result<()> {
    let socket = socket();
    let (code, resp) = wire::post_json(&socket, path, body)?;
    if code != 200 {
        bail!("fleetd ({code}): {resp}");
    }
    println!("{resp}");
    Ok(())
}

/// Parse `--iid <N>` from a verb's args — in a cog, its own MR when omitted.
fn require_iid(args: &[String], verb: &str) -> Result<i64> {
    let mut iid = None;
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        match flag.as_str() {
            "--iid" => iid = Some(it.next().cloned().ok_or_else(|| anyhow!("--iid needs a value"))?),
            other => bail!("{verb}: unknown flag {other:?}"),
        }
    }
    let iid = iid.map(|i| i.parse().map_err(|_| anyhow!("{verb}: --iid must be an integer, got {i:?}"))).transpose()?;
    iid_or_own(iid, verb)
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

/// `fleetc project` — print the project checkout's path (fleet.toml `[project] path`, as fleetd
/// resolved it). How skills (fleet-berth) find the project from any cwd, for anyone.
fn project() -> Result<()> {
    let socket = socket();
    let (code, body) = wire::get_json(&socket, "/project")?;
    if code != 200 {
        bail!("fleetd ({code}): {body}");
    }
    println!("{body}");
    Ok(())
}

/// `fleetc settings list` — fetch every live lever's effective value + source and render a table.
fn settings_list() -> Result<()> {
    let socket = socket();
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
    let mut since = None;
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        let mut take = || it.next().cloned().ok_or_else(|| anyhow!("{flag} needs a value"));
        match flag.as_str() {
            "--since" => since = Some(take()?),
            other => bail!("unknown flag: {other}"),
        }
    }
    let db = project_dir().telemetry_db();
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

    #[test]
    fn full_dump_heads_every_table_and_cuts_the_event_log() {
        let script = full_dump("events\ntickets\n");
        assert!(script.contains("'── tickets · ' || count(*) || ' rows' FROM \"tickets\""), "{script}");
        assert!(script.contains("SELECT * FROM \"tickets\";"));
        assert!(script.contains("ORDER BY seq DESC LIMIT 50;") && script.contains("rows (newest 50)"));
        assert_eq!(script.matches(".mode box --wrap 60").count(), 2);
    }

    #[test]
    fn verb_usage_is_every_form_of_that_verb() {
        let ci = verb_usage("ci").unwrap();
        assert!(ci.starts_with("usage: fleetc") && ci.contains("fleetc ci       [--iid <N>]"), "{ci}");
        assert_eq!(ci.lines().count(), 2, "the header + its one form");
        assert_eq!(verb_usage("convoy").unwrap().lines().count(), 3, "both convoy forms");
        assert!(!verb_usage("ticket").unwrap().contains("fleetc propose"), "only that verb");
        assert_eq!(verb_usage("nope"), None);
    }

    #[test]
    fn a_proposal_renders_its_provenance_then_its_body() {
        let proposal = |id, proposed| domain::Proposal {
            id: domain::ProposalId(id),
            from_ticket: Some(domain::TicketKey("PROJ-1".into())),
            proposed,
            created_at: "t".into(),
            ended_at: None,
            accepted_as: None,
        };
        let new = proposal(3, domain::Proposed::New(domain::TicketDraft {
            title: "Retry the flaky upload".into(),
            kind: "bug".into(),
            description: "## Context\nIt flakes.\n".into(),
            convoy: Some(domain::ConvoyKey("PROJ-9".into())),
            blocked_by: vec![],
            labels: vec![],
        }));
        let existing = proposal(4, domain::Proposed::Existing {
            ticket: domain::TicketKey("PROJ-7".into()),
            why: "blocks the merge".into(),
            convoy: Some(domain::ConvoyKey("PROJ-9".into())),
        });
        assert_eq!(render_proposal(&new), "proposal 3 · bug · t · from PROJ-1 · convoy PROJ-9\nRetry the flaky upload\n\n## Context\nIt flakes.\n");
        assert_eq!(render_proposal(&existing), "proposal 4 · existing PROJ-7 · t · from PROJ-1 · convoy PROJ-9\nblocks the merge\n\n(the ticket itself: fleetc ticket PROJ-7)\n");
        assert_eq!(
            render_proposals(&[new, existing]),
            "3    PROJ-1       PROJ-9       bug    Retry the flaky upload\n4    PROJ-1       PROJ-9       exists PROJ-7 · blocks the merge\n"
        );
        assert_eq!(render_proposals(&[]), "no open proposals\n");
    }

    /// A throwaway project folder with a populated `.fleet/ledger.db`.
    fn project(tag: &str) -> (wire::Project, std::path::PathBuf) {
        let root = std::env::temp_dir().join(format!("fleetc-import-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(".fleet")).unwrap();
        let p = wire::Project { root: root.clone() };
        let db = ledger::Ledger::open(&p.ledger()).unwrap();
        db.setting_set("poll_secs", "7").unwrap();
        (p, root)
    }

    #[test]
    fn import_restores_a_snapshot_and_drops_the_stale_wal() {
        let (p, root) = project("ok");
        let export = root.join("snapshot.db");
        ledger::snapshot(&p.ledger(), &export.to_string_lossy()).unwrap();
        ledger::Ledger::open(&p.ledger()).unwrap().setting_set("poll_secs", "99").unwrap(); // the state moves on
        std::fs::write(format!("{}-wal", p.ledger()), b"stale").unwrap();
        std::os::unix::net::UnixListener::bind(p.socket()).map(drop).unwrap(); // a crash-leftover socket file

        import_into(&p, &export).unwrap();
        assert!(!std::path::Path::new(&format!("{}-wal", p.ledger())).exists(), "the stale WAL is gone");
        let restored = ledger::Ledger::open_readonly(&p.ledger()).unwrap();
        assert_eq!(restored.setting_get("poll_secs").unwrap().as_deref(), Some("7"), "back to the snapshot");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn import_refuses_while_fleetd_answers_or_on_another_schema() {
        let (p, root) = project("refuse");
        let export = root.join("snapshot.db");
        ledger::snapshot(&p.ledger(), &export.to_string_lossy()).unwrap();

        let listening = std::os::unix::net::UnixListener::bind(p.socket()).unwrap(); // fleetd "running"
        let e = import_into(&p, &export).unwrap_err().to_string();
        assert!(e.contains("fleetd is running"), "{e}");
        drop(listening);
        let _ = std::fs::remove_file(p.socket());

        let other = rusqlite_version(&export, ledger::SCHEMA_VERSION + 1);
        let e = import_into(&p, &export).unwrap_err().to_string();
        assert!(e.contains(&format!("schema v{other}")), "{e}");
        let _ = std::fs::remove_dir_all(root);
    }

    /// Re-stamp a database file's schema version (simulating an export from another build).
    fn rusqlite_version(path: &std::path::Path, v: i64) -> i64 {
        let out = std::process::Command::new("sqlite3").arg(path).arg(format!("PRAGMA user_version = {v};")).output().unwrap();
        assert!(out.status.success());
        v
    }

    #[test]
    fn renders_a_ticket_as_text() {
        let d = serde_json::json!({
            "ticket": {"key": "ARMT-4", "title": "Add neg", "status": "todo", "assignee": null, "convoy": "ARMT-1", "blocked_by": ["ARMT-3"]},
            "kind": "story",
            "description": "## Outcome\n\nneg works.\n"
        });
        assert_eq!(render_ticket(&d), "ARMT-4 · story · todo\nAdd neg\nconvoy ARMT-1 · blocked by ARMT-3\n\n## Outcome\n\nneg works.\n");
    }
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
                { "action": "spawn", "ticket": "PROJ-1", "kind": "worker" },
                { "action": "launch_captain", "convoy": "PROJ-100" },
            ]
        })));
        assert!(out.contains("(dry-run)"));
        assert!(out.contains("spawn") && out.contains("PROJ-1") && out.contains("worker"));
        assert!(out.contains("launch_captain") && out.contains("PROJ-100"), "convoy actions render too");
    }

    #[test]
    fn require_ticket_rejects_flags_and_non_tickets() {
        assert_eq!(require_ticket(&["PROJ-123".into()], "anchor").unwrap(), "PROJ-123");
        // a mistyped flag swallowed as a "ticket" is the bug — must error, not no-op.
        let e = require_ticket(&["--cog".into()], "anchor").unwrap_err().to_string();
        assert!(e.contains("unexpected flag"), "{e}");
        // typos fail loudly too.
        assert!(require_ticket(&["proj-5".into()], "sink").is_err(), "lowercase");
        assert!(require_ticket(&["PROJ-".into()], "sink").is_err(), "no number");
        assert!(require_ticket(&["PROJ-12x".into()], "sink").is_err(), "trailing junk");
        assert!(require_ticket(&[], "weigh").is_err(), "missing");
    }

    #[test]
    fn require_cog_target_validates_cog_and_per_cog_key() {
        let a = |args: &[&str]| {
            require_cog_target(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>(), "anchor")
        };
        // valid: each cog with its right key.
        assert_eq!(a(&["--cog", "worker", "--ticket", "PROJ-1"]).unwrap().cog, "worker");
        assert_eq!(
            a(&["--cog", "captain", "--convoy", "PROJ-100"]).unwrap().convoy.as_deref(),
            Some("PROJ-100")
        );
        assert!(a(&["--cog", "lookout"]).unwrap().ticket.is_none());
        // errors — all loud, never silent.
        assert!(a(&["--ticket", "PROJ-1"]).is_err(), "missing --cog");
        assert!(a(&["--cog", "worker"]).is_err(), "worker needs --ticket");
        assert!(a(&["--cog", "captain"]).is_err(), "captain needs --convoy");
        assert!(a(&["--cog", "lookout", "--ticket", "PROJ-1"]).is_err(), "lookout takes no key");
        assert!(a(&["--cog", "worker", "--convoy", "PROJ-100"]).is_err(), "worker can't use --convoy");
        assert!(a(&["--cog", "foreman", "--ticket", "PROJ-1"]).is_err(), "unknown cog");
        // anchor/weigh reach a ticketed cog only through its ticket.
        let t = |args: &[&str]| ticketless_target(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>(), "anchor");
        assert!(t(&["--cog", "lookout"]).is_ok());
        assert!(t(&["--cog", "worker", "--ticket", "PROJ-1"]).unwrap_err().to_string().contains("fleetc anchor <KEY-N>"));
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
            "applied": [ { "action": "reap", "ticket": "PROJ-2" } ],
            "failed": [ { "action": { "action": "spawn", "ticket": "PROJ-3" }, "error": "no worktree" } ]
        })));
        assert!(out.contains("(live)"));
        assert!(out.contains("✓") && out.contains("reap") && out.contains("PROJ-2"));
        assert!(out.contains("✗") && out.contains("PROJ-3") && out.contains("no worktree"));
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
        let s = sess(CrewKind::Worker, Some("PROJ-1"), None, "/wt/PROJ-1", Some("abc-123"));
        let got = transcript_path(&s).unwrap();
        let want = format!("{}/.claude/projects/-wt-PROJ-1/abc-123.jsonl", std::env::var("HOME").unwrap());
        assert_eq!(got.to_string_lossy(), want);
    }

    #[test]
    fn matches_target_keys_on_ticket_convoy_or_singleton() {
        let ct = |cog: &str, ticket: Option<&str>, convoy: Option<&str>| wire::CogTarget {
            cog: cog.into(),
            ticket: ticket.map(Into::into),
            convoy: convoy.map(Into::into),
        };
        assert!(matches_target(&sess(CrewKind::Worker, Some("PROJ-1"), None, "/wt", None), &ct("worker", Some("PROJ-1"), None)));
        assert!(!matches_target(&sess(CrewKind::Worker, Some("PROJ-2"), None, "/wt", None), &ct("worker", Some("PROJ-1"), None)));
        assert!(matches_target(&sess(CrewKind::Captain, None, Some("PROJ-100"), "/project", None), &ct("captain", None, Some("PROJ-100"))));
        assert!(matches_target(&sess(CrewKind::Lookout, None, None, "/project", None), &ct("lookout", None, None)));
    }

    #[test]
    fn parse_send_takes_the_target_flags_and_the_message_positional() {
        let a = |args: &[&str]| parse_send(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        let p = a(&["--cog", "worker", "--ticket", "PROJ-1", "handle the edge case"]).unwrap();
        assert_eq!(
            (p.cog.as_str(), p.ticket.as_deref(), p.convoy, p.message.as_str()),
            ("worker", Some("PROJ-1"), None, "handle the edge case")
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
