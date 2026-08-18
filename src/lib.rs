pub mod accounts;
mod addons;
mod bottle;
mod core;
pub mod credentials;
pub mod error;
pub mod library;
mod operation;
mod prefix;
pub mod profile;
mod profiles;
mod runner;
pub mod steam;
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
pub use error::Error;
pub use library::{Library, LibraryItem, SearchAction, SearchEntry};
pub use operation::{Operation, Progress, Stage, Transfer};
pub use profiles::{
    AccountIdentity, Profile, ProfileError, Profiles, StorefrontAccount, StorefrontAccountProvider,
    StorefrontProvider,
};
pub use utils::directories::Directories;
pub use utils::environment::Environment;

pub(crate) use next_proto::winebridge as proto;
pub(crate) use utils::context::Context;
