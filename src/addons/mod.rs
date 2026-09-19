//! Cataloging, downloading, and selecting bottle addons.
//!
//! Addons have two representations:
//!
//! - [`CatalogEntry`] describes a release advertised by a remote catalog.
//! - [`Addon`] is a frozen definition containing metadata and a complete recipe,
//!   stored alongside the shared payload and embedded in owner state.
//!
//! Obtain the shared [`Addons`] manager from [`crate::Bottles::addons`]. Catalog
//! queries use the last successfully loaded catalog, while release queries expose
//! downloaded or imported releases. Fetching an entry only places it in shared
//! storage; select components and append dependencies through the owner's
//! [`crate::Edit`] callback.

#![warn(missing_docs)]

mod addon;
mod catalog;
mod error;
mod installer;
mod manager;
mod recipe;
mod recipes;

pub use addon::{Addon, Component, Dependency, Requirement, Slot};
pub use catalog::CatalogEntry;
pub(crate) use catalog::{AddonFamily, Checksum};
pub use error::{AddonError, CatalogError, InstallerError};
pub(crate) use installer::{InstallInputs, execute, uninstall};
pub use manager::Addons;
