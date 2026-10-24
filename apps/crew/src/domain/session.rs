use super::agent::Agent;

#[derive(Clone)]
pub enum SessionState {
    Working,
    Done,
    Waiting,
    Idle,
    Live,
    Dead,
    Unknown,
}

#[derive(Clone)]
pub struct Session {
    pub agent: Agent,
    pub state: SessionState,
}

impl From<&str> for SessionState {
    fn from(word: &str) -> Self {
        match word.trim() {
            "working" => Self::Working,
            "done"    => Self::Done,
            "waiting" => Self::Waiting,
            "idle"    => Self::Idle,
            _         => Self::Unknown,
        }
    }
}
