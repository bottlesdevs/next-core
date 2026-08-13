//! Cataloging, downloading, and selecting bottle addons.
//!
//! Addons pass through three representations:
//!
//! - [`CatalogEntry`] describes a release advertised by a remote catalog.
//! - [`IndexEntry`] describes a downloaded or hand-placed release in shared
//!   storage. Dependency entries retain the artifacts needed for installation.
//! - [`Addon`] is the artifact-free selection persisted in a
//!   [`crate::BottleState`].
//!
//! Obtain the shared [`Addons`] manager from [`crate::Bottles::addons`]. Catalog
//! queries use the last successfully loaded catalog, while index queries expose
//! locally available releases. Fetching an entry only places it in shared
//! storage; select components with [`crate::Bottle::set_component`] and install
//! dependencies with [`crate::Bottle::install`].

#![warn(missing_docs)]

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
