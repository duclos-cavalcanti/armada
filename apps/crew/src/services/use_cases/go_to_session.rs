use anyhow::Result;

use crate::services::{RegistryRepository, SessionHost};
use crate::domain::{Agent, Registry};

use std::str::FromStr;

enum Target {
    Index(usize), 
    Next, 
    Prev, 
    Name(String)
}

impl FromStr for Target {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s{
            "next" => Target::Next,
            "prev" => Target::Prev,
            _ => match s.parse::<usize>() {
                Ok(n) => Target::Index(n),
                Err(_) => Target::Name(s.to_string())
            },
        })
    }
}

fn current_index(registry: &Registry, host: &dyn SessionHost) -> Option<usize> {
    let current = host.current()?;
    registry.position(&current.name)
}

pub fn go_to_session(
    repo: &dyn RegistryRepository,
    host: &dyn SessionHost,
    target: String,
) -> Result<Option<Agent>> {
    let registry = repo.load()?;
    let agent = match Target::from_str(&target)? {
        Target::Index(i) => i.checked_sub(1).and_then(|i| registry.get(i)),
        Target::Name(s) => registry.position(&s).and_then(|i| registry.get(i)),
        Target::Next => current_index(&registry, host).and_then(|i| registry.next(i)),
        Target::Prev => current_index(&registry, host).and_then(|i| registry.prev(i)),
    };
    Ok(agent)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeRepo(Registry);
    impl RegistryRepository for FakeRepo {
        fn load(&self) -> Result<Registry> {
            Ok(self.0.clone())
        }
        fn save(&self, _registry: &Registry) -> Result<()> {
            Ok(())
        }
    }

    // Reports where the client currently sits (or None = not in tmux).
    struct FakeHost(Option<Agent>);
    impl SessionHost for FakeHost {
        fn current(&self) -> Option<Agent> {
            self.0.clone()
        }
        fn is_alive(&self, _name: &str) -> bool {
            true
        }
    }

    fn roster() -> FakeRepo {
        FakeRepo(Registry::from_agents(vec![
            Agent::new("a", "/a"),
            Agent::new("b", "/b"),
            Agent::new("c", "/c"),
        ]))
    }

    // Current session = the named one from the roster.
    fn on(name: &str) -> FakeHost {
        FakeHost(Some(Agent::new(name, "/x")))
    }

    fn go(repo: &FakeRepo, host: &FakeHost, target: &str) -> Option<String> {
        go_to_session(repo, host, target.to_string())
            .unwrap()
            .map(|a| a.name)
    }

    #[test]
    fn index_is_one_based() {
        assert_eq!(go(&roster(), &on("a"), "2"), Some("b".to_string()));
    }

    #[test]
    fn index_zero_and_out_of_range_are_none() {
        assert_eq!(go(&roster(), &on("a"), "0"), None);
        assert_eq!(go(&roster(), &on("a"), "9"), None);
    }

    #[test]
    fn name_resolves_to_that_agent() {
        assert_eq!(go(&roster(), &on("a"), "c"), Some("c".to_string()));
    }

    #[test]
    fn unknown_name_is_none() {
        assert_eq!(go(&roster(), &on("a"), "ghost"), None);
    }

    #[test]
    fn next_advances_and_wraps() {
        assert_eq!(go(&roster(), &on("a"), "next"), Some("b".to_string()));
        assert_eq!(go(&roster(), &on("c"), "next"), Some("a".to_string())); // wrap
    }

    #[test]
    fn prev_retreats_and_wraps() {
        assert_eq!(go(&roster(), &on("c"), "prev"), Some("b".to_string()));
        assert_eq!(go(&roster(), &on("a"), "prev"), Some("c".to_string())); // wrap
    }

    #[test]
    fn next_prev_are_noop_when_off_roster() {
        // current session is not in the registry
        assert_eq!(go(&roster(), &on("ghost"), "next"), None);
        // not in tmux at all
        assert_eq!(go(&roster(), &FakeHost(None), "prev"), None);
    }

    #[test]
    fn index_and_name_work_even_when_off_roster() {
        // Index/Name don't depend on where you currently are.
        assert_eq!(go(&roster(), &FakeHost(None), "2"), Some("b".to_string()));
        assert_eq!(go(&roster(), &on("ghost"), "a"), Some("a".to_string()));
    }
}


