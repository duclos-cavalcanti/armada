use crossterm::event::KeyCode;

pub fn is_quit(code: KeyCode) -> bool {
    matches!(code, KeyCode::Char('q'))
}
