#![doc = include_str!("../README.md")]
#![deny(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]
#![warn(rustdoc::all)]

mod addons;
mod command;
mod core;
pub mod error;
mod operation;
mod prefix;
mod prefixes;
mod program_spec;
pub use program_spec::ProgramSpec;
mod runner;
mod utils;
#[cfg(feature = "fvs")]
mod virgo;
/// Stands in for the FVS services when the `fvs` feature is disabled.
///
/// The placeholder lets FVS handles be stored and passed without feature gates.
/// It gives no access to FVS; every use of FVS is compiled only with the feature.
#[cfg(not(feature = "fvs"))]
mod virgo {
    use crate::error::Result;
    use std::path::PathBuf;

    pub(crate) struct Fvs;

    impl Fvs {
        pub(crate) async fn connect(_executable: Option<PathBuf>) -> Result<Self> {
            Ok(Self)
        }
    }
}
mod winebridge;

pub use addons::{
    Addon, AddonError, AddonRole, AddonSettings, AddonSettingsSchema, Addons, AddonsState,
    CatalogEntry, CatalogError, Catalogs, CatalogsState, Category, Component, Control, Dependency,
    InstalledAddon, InstallerError, Requirement, RoleMarker, Runner, RunnerFlavor, SchemaError,
    SelectOption, Setting, SettingValue, Slot, Umu, WineBridge, compare_versions,
};
pub use command::wrappers::{
    GamescopeConfig, GamescopeFilter, GamescopeScaler, MangoHudConfig, Wrappers,
};
pub use core::{Core, CoreBuilder};
pub use operation::{Job, Operation, Progress, Stage, Transfer};
pub use prefix::{Edit, Inventory, Prefix, PrefixBackend, PrefixError, PrefixState};
#[cfg(feature = "fvs")]
pub use prefix::{Snapshot, SnapshotSummary};
pub use prefixes::Prefixes;
pub use proto::{DllOverride, DllOverrideMode, Process, RegistryHive};
pub use utils::env_vars::EnvVars;

pub(crate) use next_proto::winebridge as proto;
