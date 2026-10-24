use std::fs;
use std::path::PathBuf;

use anyhow::Result;

use crate::domain::{Agent, Registry};
use crate::services::RegistryRepository;

pub struct FsRegistryRepository {
    path: PathBuf,
}

impl FsRegistryRepository {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

impl RegistryRepository for FsRegistryRepository {
    fn load(&self) -> Result<Registry> {
        let text = fs::read_to_string(&self.path)?;
        let agents = text
            .lines()
            .filter(|line| !line.trim().is_empty())
            .filter_map(|line| line.split_once('='))
            .map(|(name, path)| Agent::new(name, path))
            .collect();
        Ok(Registry::from_agents(agents))
    }

    fn save(&self, registry: &Registry) -> Result<()> {
        let tmp = self.path.with_extension("tmp");
        let text = registry
            .agents()
            .iter()
            .map(|a| format!("{}={}", a.name, a.path))
            .collect::<Vec<_>>() 
            .join("\n");
        fs::write(&tmp, text)?;
        fs::rename(&tmp, &self.path)?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    // A throwaway registry path unique to this process, 
    // Parallel test binaries don't clobber each other.
    fn temp_path(tag: &str) -> PathBuf {
        env::temp_dir().join(format!("crew-test-{}-{}", tag, std::process::id()))
    }

    #[test]
    fn save_then_load_round_trips() {
        let path = temp_path("roundtrip");
        let repo = FsRegistryRepository::new(&path);

        let registry = Registry::from_agents(vec![
            Agent::new("millwright", "/a"),
            Agent::new("prover", "/b"),
        ]);
        repo.save(&registry).expect("save should succeed");

        let loaded = repo.load().expect("load should succeed");
        let got: Vec<(String, String)> = loaded
            .agents()
            .iter()
            .map(|a| (a.name.clone(), a.path.clone()))
            .collect();
        assert_eq!(
            got,
            vec![
                ("millwright".to_string(), "/a".to_string()),
                ("prover".to_string(), "/b".to_string()),
            ]
        );

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn save_overwrites_existing_roster() {
        let path = temp_path("overwrite");
        let repo = FsRegistryRepository::new(&path);

        repo.save(&Registry::from_agents(vec![Agent::new("old", "/x")]))
            .unwrap();
        repo.save(&Registry::from_agents(vec![Agent::new("new", "/y")]))
            .unwrap();

        let loaded = repo.load().unwrap();
        assert_eq!(loaded.agents().len(), 1);
        assert_eq!(loaded.agents()[0].name, "new");

        let _ = fs::remove_file(&path);
    }
}
