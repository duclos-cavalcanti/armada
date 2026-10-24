use std::fs;
use std::io::ErrorKind;
use std::path::PathBuf;

use anyhow::Result;

use crate::services::StateStore;

pub struct FsStateStore {
    dir: PathBuf,
}

impl FsStateStore {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }
}

impl StateStore for FsStateStore {
    fn read_state(&self, name: &str) -> Result<Option<String>> {
        let file = self.dir.join(name);
        match fs::read_to_string(&file) {
            Ok(text) => Ok(Some(text.trim().to_string())),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
}
