use anyhow::Result;

use crate::services::{RegistryRepository};

pub fn rm_bookmark(repo: &dyn RegistryRepository, target: String) -> Result<()> {
    let mut registry = repo.load()?;
    let removed = match target.parse::<usize>() {
        Ok(n) => n.checked_sub(1).and_then(|i| registry.rm(i)),
        Err(_) => {
            let i = registry
                .position(&target)
                .ok_or_else(|| anyhow::anyhow!("no bookmark named {target}"))?;
            registry.rm(i)
        }
    };
    if removed.is_some() {
        repo.save(&registry)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Agent, Registry};
    use std::cell::RefCell;

    // Same capturing fake as add_bookmark: load() reads, save() overwrites.
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

    fn repo_with(names: &[&str]) -> FakeRepo {
        FakeRepo(RefCell::new(Registry::from_agents(
            names.iter().map(|n| Agent::new(*n, "/path")).collect(),
        )))
    }

    #[test]
    fn removes_by_one_based_index() {
        let repo = repo_with(&["millwright", "prover"]);
        rm_bookmark(&repo, "1".to_string()).unwrap();
        let left = repo.load().unwrap();
        assert_eq!(left.agents().len(), 1);
        assert_eq!(left.agents()[0].name, "prover"); // "1" removed the first entry
    }

    #[test]
    fn removes_by_name() {
        let repo = repo_with(&["millwright", "prover"]);
        rm_bookmark(&repo, "prover".to_string()).unwrap();
        let left = repo.load().unwrap();
        assert_eq!(left.agents().len(), 1);
        assert_eq!(left.agents()[0].name, "millwright");
    }

    #[test]
    fn unknown_name_errors_and_leaves_roster_untouched() {
        let repo = repo_with(&["millwright"]);
        assert!(rm_bookmark(&repo, "ghost".to_string()).is_err());
        assert_eq!(repo.load().unwrap().agents().len(), 1);
    }

    #[test]
    fn out_of_range_index_is_a_noop() {
        let repo = repo_with(&["millwright"]);
        rm_bookmark(&repo, "9".to_string()).unwrap();
        assert_eq!(repo.load().unwrap().agents().len(), 1);
    }

    #[test]
    fn index_zero_is_a_noop() {
        let repo = repo_with(&["millwright"]);
        rm_bookmark(&repo, "0".to_string()).unwrap(); // 1-based; 0 -> checked_sub -> None
        assert_eq!(repo.load().unwrap().agents().len(), 1);
    }
}

