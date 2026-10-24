use anyhow::Result;

pub trait StateStore {
    fn read_state(&self, name: &str) -> Result<Option<String>>;
}
