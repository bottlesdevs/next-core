//! Catalogs, acquires, and describes software used by Bottles prefixes.
//!
//! Addons have two lifecycle representations:
//!
//! - [`CatalogEntry`] advertises a remote release and its platform artifacts.
//! - [`Addon`] describes a locally acquired release with frozen metadata and,
//!   for prefix software, an installation recipe.
//!
//! [`Runner`], [`WineBridge`], and [`Umu`] supply runtime tooling through dedicated
//! prefix fields. [`Component`] releases occupy mutually exclusive [`Slot`]s; [`Dependency`]
//! releases are appended in installation order. A release may declare
//! [`Requirement`]s that prefix validation must satisfy.
//!
//! [`AddonRole`] holds one marker, and [`RoleMarker`] bounds the type parameter
//! of [`Addon`], so one generic API acquires, imports, and lists every role.
//!
//! [`Addons`], from [`crate::Core::addons`], stores locally acquired releases.
//! [`Catalogs`], from [`crate::Core::catalogs`], refreshes remote catalogs and
//! fetches their releases into [`Addons`]. Acquiring a release only places it in
//! shared storage. Selection remains an explicit prefix edit through
//! [`crate::Edit`].

#![warn(missing_docs)]

mod addon;
mod catalog;
mod catalogs;
mod defaults;
mod error;
mod installed;
mod installer;
mod recipe;
mod settings;
mod store;

pub use addon::{
    Addon, AddonRole, Component, Dependency, Requirement, RoleMarker, Runner, RunnerFlavor, Slot,
    Umu, WineBridge,
};
pub use catalog::CatalogEntry;
pub use catalogs::{Catalogs, CatalogsState};
pub use error::{AddonError, CatalogError, InstallerError};
pub use installed::InstalledAddon;
pub(crate) use installer::{InstallInputs, execute, uninstall};
pub(crate) use recipe::InstallStep;
pub use settings::{
    AddonSettings, AddonSettingsSchema, Category, Control, SchemaError, SelectOption, Setting,
    SettingValue,
};
pub use store::{Addons, AddonsState};
