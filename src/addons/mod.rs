//! Discovering, downloading, and describing bottle addons.
//!
//! Obtain the shared [`Addons`] manager from [`crate::Bottles::addons`]. The
//! manager exposes typed Bottles-maintained [`CatalogEntry`] values separately
//! from downloaded and hand-placed [`IndexEntry`] values.
//!
//! Fetching only places resources in library-managed storage. Bottles select
//! components with [`crate::Bottle::set_component`] and install dependencies
//! with [`crate::Bottle::install`].

use serde::{Deserialize, Deserializer, de};

mod addon;
mod catalog;
mod error;
mod index;
mod installer;
mod manager;

pub use addon::{Addon, Component, Dependency, Requirement, Slot};
pub use catalog::CatalogEntry;
pub(crate) use catalog::Checksum;
pub use error::{AddonError, CatalogError, InstallerError};
pub use index::IndexEntry;
pub(crate) use installer::{Artifact, InstallInputs, execute, replay_environment, uninstall};
pub use manager::Addons;

/// Rejects empty or whitespace-only input without trimming accepted values.
pub(crate) fn deserialize_non_empty_string<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    let value = String::deserialize(deserializer)?;

    if value.trim().is_empty() {
        return Err(de::Error::custom("value cannot be empty"));
    }

    Ok(value)
}
