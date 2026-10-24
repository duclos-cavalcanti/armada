use crate::domain::Agent;
use crate::services::SessionHost;

pub struct TmuxHost;

impl TmuxHost {
    fn has_session(name: &str) -> bool {
        std::process::Command::new("tmux")
            .args(["has-session", "-t", &format!("={name}")])
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    fn current_session() -> Option<Agent> {
        let out=std::process::Command::new("tmux")
            .args(["display", "-p", "#{session_name}=#{session_path}"])
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let text = String::from_utf8(out.stdout).ok()?;
        let text = text.trim();
        if text.is_empty() {
            None
        } else {
            let (name, path) = text.split_once("=")?;
            Some(Agent {name: name.to_string(), path: path.to_string()})
        }
    }
}

impl SessionHost for TmuxHost {
    fn is_alive(&self, name: &str) -> bool {
        Self::has_session(name)
    }

    fn current(&self) -> Option<Agent> {
        Self::current_session()
    }
}
