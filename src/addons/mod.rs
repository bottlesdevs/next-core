//! Cataloging, downloading, and selecting bottle addons.
//!
//! Addons pass through three representations:
//!
//! - [`CatalogEntry`] describes a release advertised by a remote catalog.
//! - [`Release`] describes a local release with its frozen recipe and source payload.
//! - [`Addon`] is the artifact-free selection persisted in a
//!   [`crate::BottleState`].
//!
//! Obtain the shared [`Addons`] manager from [`crate::Bottles::addons`]. Catalog
//! queries use the last successfully loaded catalog, while release queries expose
//! downloaded or imported releases. Fetching an entry only places it in shared
//! storage; select components with [`crate::Bottle::set_component`] and install
//! dependencies with [`crate::Bottle::install`].

#![warn(missing_docs)]

use serde::{Deserialize, Deserializer, de};

mod addon;
mod catalog;
mod error;
mod installer;
mod manager;
mod release;

pub use addon::{Addon, Component, Dependency, Requirement, Slot};
pub use catalog::CatalogEntry;
pub(crate) use catalog::Checksum;
pub use error::{AddonError, CatalogError, InstallerError};
pub(crate) use installer::{InstallInputs, execute, uninstall};
pub use manager::Addons;
pub use release::Release;

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
