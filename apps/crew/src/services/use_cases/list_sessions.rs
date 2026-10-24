use anyhow::Result;

use crate::domain::{Session, SessionState};
use crate::services::{RegistryRepository, StateStore, SessionHost};

pub fn list_sessions(repo: &dyn RegistryRepository, store: &dyn StateStore, host: &dyn SessionHost) -> Result<Vec<Session>> {
    let registry = repo.load()?;
    let sessions = registry
        .agents()
        .into_iter()
        .map(|agent| {
            let state = match store.read_state(&agent.name)? {
                Some(s) => SessionState::from(s.as_str()), 
                None => if host.is_alive(&agent.name) {
                    SessionState::Live
                } else {
                    SessionState::Dead
                }
            };
            Ok(Session {
                agent: agent.clone(),
                state
            })
        })
        .collect();
    sessions
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Agent, Registry};

    struct DummyRepository(Registry);

    impl RegistryRepository for DummyRepository {
        fn load(&self) -> Result<Registry> {
            Ok(self.0.clone())
        }
        fn save(&self, _registry: &Registry) -> Result<()> {
            Ok(())
        }
    }

    struct FixedStore(Option<String>);
    impl StateStore for FixedStore {
        fn read_state(&self, _name: &str) -> Result<Option<String>> {
            Ok(self.0.clone())
        }
    }

    struct DummyHost(bool);
    impl SessionHost for DummyHost {
        fn current(&self) -> Option<Agent> {
            None
        }
        fn is_alive(&self, _name: &str) -> bool {
            self.0
        }
    }

    fn one_agent() -> DummyRepository {
        DummyRepository(Registry::from_agents(vec![Agent::new("millwright", "/a")]))
    }

    #[test]
    fn maps_agents_to_sessions() {
        let repo = DummyRepository(Registry::from_agents(vec![
            Agent::new("millwright", "/a"),
            Agent::new("prover", "/b"),
        ]));

        let sessions =
            list_sessions(&repo, &FixedStore(None), &DummyHost(false)).expect("should succeed");

        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].agent.name, "millwright");
    }

    #[test]
    fn no_state_file_falls_back_to_host_liveness() {
        let alive = list_sessions(&one_agent(), &FixedStore(None), &DummyHost(true)).unwrap();
        assert!(matches!(alive[0].state, SessionState::Live));

        let dead = list_sessions(&one_agent(), &FixedStore(None), &DummyHost(false)).unwrap();
        assert!(matches!(dead[0].state, SessionState::Dead));
    }

    #[test]
    fn recorded_state_word_overrides_host() {
        let sessions = list_sessions(
            &one_agent(),
            &FixedStore(Some("working".into())),
            &DummyHost(false),
        )
        .unwrap();
        assert!(matches!(sessions[0].state, SessionState::Working));
    }
}
