//! Catalogs, acquires, and describes software used by Bottles environments.
//!
//! Addons have two lifecycle representations:
//!
//! - [`CatalogEntry`] advertises a remote release and its platform artifacts.
//! - [`Addon`] describes a locally acquired release with a frozen installation
//!   recipe.
//!
//! [`Component`] releases occupy mutually exclusive [`Slot`]s; [`Dependency`]
//! releases are appended in installation order. A release may declare
//! [`Requirement`]s that environment validation must satisfy.
//!
//! Use [`crate::Bottles::addons`] to access the shared [`Addons`] manager. Acquiring
//! a release only places it in shared storage. Selection remains an explicit owner
//! edit through [`crate::Edit::set_component`] or [`crate::Edit::add_dependency`].

#![warn(missing_docs)]

mod addon;
mod catalog;
mod defaults;
mod error;
mod installer;
mod manager;
mod recipe;

pub use addon::{Addon, Component, Dependency, Requirement, Slot};
pub(crate) use catalog::AddonFamily;
pub use catalog::CatalogEntry;
pub use error::{AddonError, CatalogError, InstallerError};
pub(crate) use installer::{InstallInputs, execute, uninstall};
pub use manager::Addons;
