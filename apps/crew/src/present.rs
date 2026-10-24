#![allow(dead_code)]

use crate::domain::{SessionState, Session};

fn glyph(state: &SessionState) -> &'static str {
    match state {
        SessionState::Working => "»",
        SessionState::Done => "⏸",
        SessionState::Waiting => "⏸",
        SessionState::Idle => "●",
        SessionState::Live => "●",
        SessionState::Dead => "○",
        SessionState::Unknown => "○",
    }
}

fn color(state: &SessionState) -> &'static str {
    match state {
        SessionState::Working => "green",
        SessionState::Done => "red",
        SessionState::Waiting => "white",
        SessionState::Idle => "brightblack",
        SessionState::Live => "green",
        SessionState::Dead => "brightblack",
        SessionState::Unknown => "brightblack",
    }
}

pub fn tmux(sessions: &[Session], current: Option<&str>) -> String {
    sessions
        .iter()
        .map(|s| {
            let base = format!(
                "#[fg={}]{} {}#[fg=default]",
                color(&s.state),
                glyph(&s.state),
                s.agent.name
            );
            if current == Some(s.agent.name.as_str()) {
                format!("#[underscore]{base}#[default]")
            } else {
                base
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn plain(sessions: &[Session]) -> String {
    sessions
        .iter()
        .map(|s| format!("{} {}", glyph(&s.state), s.agent.name))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Agent;

    fn session(name: &str, state: SessionState) -> Session {
        Session {
            agent: Agent::new(name, "/path"),
            state,
        }
    }

    #[test]
    fn tmux_wraps_each_entry_in_color_markup() {
        let sessions = vec![
            session("millwright", SessionState::Working),
            session("prover", SessionState::Dead),
        ];
        assert_eq!(
            tmux(&sessions, None),
            "#[fg=green]» millwright#[fg=default] #[fg=brightblack]○ prover#[fg=default]"
        );
    }

    #[test]
    fn tmux_underlines_the_current_session() {
        let sessions = vec![
            session("millwright", SessionState::Working),
            session("prover", SessionState::Idle),
        ];
        assert_eq!(
            tmux(&sessions, Some("prover")),
            "#[fg=green]» millwright#[fg=default] \
             #[underscore]#[fg=brightblack]● prover#[fg=default]#[default]"
        );
    }

    #[test]
    fn plain_is_glyph_and_name_no_color() {
        let sessions = vec![
            session("millwright", SessionState::Working),
            session("prover", SessionState::Idle),
        ];
        assert_eq!(plain(&sessions), "» millwright ● prover");
    }

    #[test]
    fn empty_input_yields_empty_string() {
        assert_eq!(tmux(&[], None), "");
        assert_eq!(plain(&[]), "");
    }
}
