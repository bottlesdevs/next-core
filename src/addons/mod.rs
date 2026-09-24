//! Catalogs, acquires, and describes software used by Bottles environments.
//!
//! Addons have two lifecycle representations:
//!
//! - [`CatalogEntry`] advertises a remote release and its platform artifacts.
//! - [`Addon`] describes a locally acquired release with frozen metadata and,
//!   for prefix software, an installation recipe.
//!
//! [`Runner`], [`WineBridge`], and [`Umu`] supply runtime tooling through dedicated
//! environment fields. [`Component`] releases occupy mutually exclusive [`Slot`]s; [`Dependency`]
//! releases are appended in installation order. A release may declare
//! [`Requirement`]s that environment validation must satisfy.
//!
//! Use [`crate::Bottles::addons`] to access the shared [`Addons`] manager. Acquiring
//! a release only places it in shared storage. Selection remains an explicit owner
//! edit through [`crate::Edit`].

#![warn(missing_docs)]

mod addon;
mod catalog;
mod defaults;
mod error;
mod installer;
mod manager;
mod recipe;

pub use addon::{Addon, Component, Dependency, Requirement, Runner, Slot, Umu, WineBridge};
pub use catalog::{AddonKind, CatalogEntry};
pub use error::{AddonError, CatalogError, InstallerError};
pub(crate) use installer::{InstallInputs, execute, uninstall};
pub(crate) use manager::StoredAddon;
pub use manager::{Addons, AddonsState};
#[cfg(feature = "fvs")]
pub(crate) use recipe::InstallResource;
pub(crate) use recipe::InstallStep;
