use anyhow::Result;

use crate::domain::Registry;

pub trait RegistryRepository {
    fn load(&self) -> Result<Registry>;
    fn save(&self, registry: &Registry) -> Result<()>;
}
