// Action keys: any non-navigation keypress over a focused row is handed to ONE
// external handler — `handler.sh`, next to this crate's Cargo.toml — which owns
// the whole (coordinate, key) → action mapping. Navigation keys are hardcoded in
// `main.rs::App::handle_key` (see `is_reserved`).

use crossterm::event::{KeyCode, KeyModifiers};

/// The action handler, resolved at compile time to the in-repo script (the
/// dashboard is always built from the repo: `make dashboard`). To change an
/// action, edit the script; the binary never changes.
const HANDLER: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/handler.sh");

/// Convert a (KeyCode, KeyModifiers) tuple to the canonical lowercase key name
/// passed to the handler. Returns `None` for keys the handler is never sent.
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

/// Keys the TUI core owns — never forwarded to the handler.
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

/// Invoke the handler in a detached background thread as
///   handler.sh <coordinate> <key> <field>...
/// — a direct exec (no `sh -c`), so row fields are distinct argv entries and need
/// no shell quoting. Fire-and-forget: stdout/stderr go to /dev/null (they'd
/// corrupt the TUI's alternate screen); an unhandled (coordinate, key) is the
/// handler's no-op.
pub fn spawn(project: &std::path::Path, coordinate: &str, key: &str, fields: Vec<String>) {
    let (project, coordinate, key) = (project.to_path_buf(), coordinate.to_string(), key.to_string());
    std::thread::spawn(move || {
        let _ = std::process::Command::new(HANDLER)
            .env("FLEET_PROJECT", &project) // the handler reaches the fleet with `fleetc -C`
            .arg(&coordinate)
            .arg(&key)
            .args(&fields)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    });
}
