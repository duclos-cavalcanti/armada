use anyhow::Result;

use crate::services::{RegistryRepository, SessionHost};

pub fn add_bookmark(repo: &dyn RegistryRepository, host: &dyn SessionHost) -> Result<()> {
    let Some(agent) = host.current() else {
        anyhow::bail!("no session on this host");
    };
    let mut registry = repo.load()?;
    registry.add(agent);
    repo.save(&registry)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Agent, Registry};
    use std::cell::RefCell;

    // Captures the roster: load() returns current contents, save() overwrites
    // them so a test can inspect what add_bookmark persisted.
    struct FakeRepo(RefCell<Registry>);
    impl RegistryRepository for FakeRepo {
        fn load(&self) -> Result<Registry> {
            Ok(self.0.borrow().clone())
        }
        fn save(&self, registry: &Registry) -> Result<()> {
            *self.0.borrow_mut() = registry.clone();
            Ok(())
        }
    }

    // Returns a fixed "current session", or None (not inside tmux).
    struct FakeHost(Option<Agent>);
    impl SessionHost for FakeHost {
        fn current(&self) -> Option<Agent> {
            self.0.clone()
        }
        fn is_alive(&self, _name: &str) -> bool {
            true
        }
    }

    #[test]
    fn adds_current_session_and_saves() {
        let repo = FakeRepo(RefCell::new(Registry::new()));
        let host = FakeHost(Some(Agent::new("millwright", "/a")));

        add_bookmark(&repo, &host).expect("should succeed");

        let saved = repo.load().unwrap();
        assert_eq!(saved.agents().len(), 1);
        assert_eq!(saved.agents()[0].name, "millwright");
        assert_eq!(saved.agents()[0].path, "/a");
    }

    #[test]
    fn dedups_when_session_already_bookmarked() {
        let repo = FakeRepo(RefCell::new(Registry::from_agents(vec![Agent::new(
            "millwright",
            "/a",
        )])));
        let host = FakeHost(Some(Agent::new("millwright", "/a")));

        add_bookmark(&repo, &host).unwrap();

        assert_eq!(repo.load().unwrap().agents().len(), 1);
    }

    #[test]
    fn errors_when_not_on_a_session() {
        let repo = FakeRepo(RefCell::new(Registry::new()));
        let host = FakeHost(None);

        assert!(add_bookmark(&repo, &host).is_err());
    }
}
