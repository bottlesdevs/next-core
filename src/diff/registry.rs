use regdiff_rs::prelude::{Diff, Hive, Registry};
use std::path::Path;

pub use regdiff_rs::prelude::Hive as RegistryHive;

pub struct RegistryDiff;

impl RegistryDiff {
    /// Computes a .reg patch between two Wine registry snapshots.
    pub fn diff(before: &Path, after: &Path, hive: Hive) -> Result<String, String> {
        let before_reg = Registry::try_from(before, hive)
            .map_err(|e| format!("Failed to load before snapshot: {}", e))?;
        let after_reg = Registry::try_from(after, hive)
            .map_err(|e| format!("Failed to load after snapshot: {}", e))?;

        let patch = Registry::diff(&before_reg, &after_reg);
        Ok(patch.serialize())
    }
}
