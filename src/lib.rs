mod addons;
mod bottle;
mod core;
pub mod error;
#[cfg(target_os = "macos")]
pub mod gptk_setup;
mod operation;
mod prefix;
mod runner;
mod utils;
mod winebridge;
mod wrapper;

pub use addons::{
    Addon, AddonError, Addons, CatalogEntry, CatalogError, Component, Dependency, IndexEntry,
    InstallerError, Requirement, Slot,
};
pub use bottle::{
    Bottle, BottleEdit, BottleManager, BottleState, DllOverride, DllOverrideMode, GamescopeConfig,
    GamescopeFilter, GamescopeScaler, MangoHudConfig, Process, Program, RegistryHive, Storage,
    Wrappers,
};
#[cfg(feature = "fvs")]
pub use bottle::{Snapshot, SnapshotSummary};
pub use core::{Bottles, Config};
pub use operation::{Operation, Progress, Stage, Transfer};
pub use utils::environment::Environment;

pub(crate) use next_proto::winebridge as proto;
pub(crate) use utils::{context::Context, directories::Directories};
