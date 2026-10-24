// Config-driven keybindings for action keys (anything that runs a shell
// command). Navigation keys (q, Esc, r, h/l, j/k, 1-4, s/S) are
// hardcoded in `main.rs::App::handle_key` and not configurable.
//
// Loading order:
//   1. `~/.mady/dashboard/dashboard.yml`  → operator override
//   2. embedded default (`dashboard.default.yml`, baked at compile time)
//
// Re-read on each `r` refresh — no file-watching.

use crossterm::event::{KeyCode, KeyModifiers};
use directories::UserDirs;
use serde::Deserialize;
use std::path::PathBuf;

/// Top-level config. The dashboard is a generic engine: it ingests state,
/// renders tabs → panes → views, and — on a non-navigation keypress over a
/// focused row — hands the coordinate, the key, and the row's fields to ONE
/// external handler. All action logic lives in that handler
/// (`bin/features/dashboard/handler.sh`), decoupled from the dashboard itself.
///
/// The only knob is the handler's path:
///   action_handler: '~/.mady/bin/features/dashboard/handler.sh'
///
/// The handler is invoked (see `spawn_action`) as:
///   <handler> <tab/pane/view> <key> <row-field>...
/// e.g.  handler.sh system/foreman/escalations enter worker MADY-123 …
///
/// A leading `~/` in the path is expanded at spawn time. The handler owns
/// the (coordinate, key) → action mapping; an unhandled pair is a no-op.
#[derive(Debug, Deserialize)]
pub struct Config {
    #[serde(default = "default_action_handler")]
    pub action_handler: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            action_handler: default_action_handler(),
        }
    }
}

fn default_action_handler() -> String {
    // `~/` is expanded by `spawn_action`, not here.
    "~/.mady/apps/dashboard/handler.sh".to_string()
}

impl Config {
    /// Load `~/.mady/dashboard/dashboard.yml` if it exists; otherwise return the
    /// embedded default. Parse errors are swallowed back to the default
    /// — the dashboard never crashes on bad user config.
    pub fn load() -> Self {
        if let Some(path) = override_path() {
            if let Ok(raw) = std::fs::read_to_string(&path) {
                if let Ok(cfg) = serde_yaml::from_str::<Config>(&raw) {
                    return cfg;
                }
            }
        }
        serde_yaml::from_str::<Config>(EMBEDDED_DEFAULT).unwrap_or_default()
    }
}

// Embedded recovery default — `tooling/mady/dashboard/dashboard.default.yml`,
// next to this source file. The live config the operator edits is
// `tooling/mady/dashboard/dashboard.yml`, reachable post-install at
// `~/.mady/dashboard/dashboard.yml` (the whole crate dir is stowed).
// If the live one is missing or unparseable, this embedded copy is
// the fallback so the dashboard never crashes on a broken YAML.
//
// Recover from a broken live config with:
//   cp tooling/mady/dashboard/dashboard.default.yml \
//      tooling/mady/dashboard/dashboard.yml
const EMBEDDED_DEFAULT: &str = include_str!("../dashboard.default.yml");

fn override_path() -> Option<PathBuf> {
    let home = UserDirs::new()?.home_dir().to_path_buf();
    // The whole `tooling/mady/dashboard/` crate is stowed into
    // `~/.mady/dashboard/`, so the live YAML resolves through that
    // symlink to the in-repo file. Operator edits go straight into
    // the repo (visible in git status, can be committed).
    Some(home.join(".mady").join("dashboard").join("dashboard.yml"))
}

/// Convert a (KeyCode, KeyModifiers) tuple to the canonical lowercase
/// string used as the YAML key. Returns `None` for keys that the config
/// surface does not address (e.g. function keys — could be added later).
///
/// Mapping:
///   Enter        → "enter"
///   Char('a')    → "a"          (case preserved for letters — Shift-A is "A")
///   Char(' ')    → "space"
///   F(n)         → "f<n>"
///   Plain Backspace / Delete / Insert / PageUp/Down / Home / End — by lowercase name
///
/// Modifier prefixes for printable keys: "ctrl+x", "alt+x". Shift is
/// inferred from the char case for letters; for other prints we leave
/// it implicit.
pub fn key_name(code: KeyCode, mods: KeyModifiers) -> Option<String> {
    let base = match code {
        KeyCode::Enter => "enter".to_string(),
        KeyCode::Backspace => "backspace".to_string(),
        KeyCode::Delete => "delete".to_string(),
        KeyCode::Insert => "insert".to_string(),
        KeyCode::Home => "home".to_string(),
        KeyCode::End => "end".to_string(),
        KeyCode::PageUp => "pageup".to_string(),
        KeyCode::PageDown => "pagedown".to_string(),
        KeyCode::F(n) => format!("f{}", n),
        KeyCode::Char(' ') => "space".to_string(),
        KeyCode::Char(c) => c.to_string(),
        _ => return None,
    };

    let mut prefix = String::new();
    if mods.contains(KeyModifiers::CONTROL) {
        prefix.push_str("ctrl+");
    }
    if mods.contains(KeyModifiers::ALT) {
        prefix.push_str("alt+");
    }
    Some(format!("{}{}", prefix, base))
}

/// Keys the TUI core owns. Config bindings for these are ignored.
pub fn is_reserved(code: KeyCode, mods: KeyModifiers) -> bool {
    if mods.contains(KeyModifiers::CONTROL) || mods.contains(KeyModifiers::ALT) {
        return false;
    }
    matches!(
        code,
        KeyCode::Esc
            | KeyCode::Up
            | KeyCode::Down
            | KeyCode::Tab
            | KeyCode::BackTab
            | KeyCode::Char('q')
            | KeyCode::Char('r')
            | KeyCode::Char('y')
            | KeyCode::Char('Y')
            | KeyCode::Char('j')
            | KeyCode::Char('k')
            | KeyCode::Char('1')
            | KeyCode::Char('2')
            | KeyCode::Char(' ')
    )
}

/// Invoke the action handler in a detached background thread as
///   <handler> <coordinate> <key> <field>...
/// — a direct exec (no `sh -c`), so row fields are passed as distinct argv
/// entries and need no shell quoting. A leading `~/` in `handler` is
/// expanded to the home dir (the OS does not expand `~` for a bare exec).
///
/// Stdout and stderr go to /dev/null; the dashboard never blocks on or
/// reports the handler's exit status — fire-and-forget. An unknown
/// (coordinate, key) is the handler's no-op, not the dashboard's concern.
pub fn spawn_action(handler: &str, coordinate: &str, key: &str, fields: Vec<String>) {
    let handler = expand_tilde(handler);
    let coordinate = coordinate.to_string();
    let key = key.to_string();
    std::thread::spawn(move || {
        let _ = std::process::Command::new(&handler)
            .arg(&coordinate)
            .arg(&key)
            .args(&fields)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    });
}

/// Expand a leading `~/` to the operator's home directory. Anything else
/// (absolute path, bare name on PATH) is returned unchanged.
fn expand_tilde(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(dirs) = UserDirs::new() {
            return dirs.home_dir().join(rest).to_string_lossy().into_owned();
        }
    }
    path.to_string()
}
