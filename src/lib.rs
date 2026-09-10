mod addons;
mod bottle;
mod core;
mod credentials;
mod environment;
pub mod error;
mod library;
mod operation;
mod plugins;
mod profiles;
mod runner;
mod utils;
mod winebridge;
mod wrapper;

pub use addons::{
    Addon, AddonError, Addons, CatalogEntry, CatalogError, Component, Dependency, IndexEntry,
    InstallerError, Requirement, Slot,
};
pub use bottle::{
    Bottle, BottleEdit, BottleError, BottleManager, BottleState, DllOverride, DllOverrideMode,
    GamescopeConfig, GamescopeFilter, GamescopeScaler, MangoHudConfig, Process, ProgramSpec,
    RegistryHive, Wrappers,
};
#[cfg(feature = "fvs")]
pub use bottle::{Snapshot, SnapshotSummary};
pub use core::{Bottles, Config};
pub use environment::{EnvironmentConfig, EnvironmentError, Storage};
pub use library::{Library, LibraryItem, SearchEntry, SearchSource};
pub use operation::{Operation, Progress, Stage, Transfer};
pub use plugins::{PluginError, PluginId, PluginInfo, PluginKind, PluginManifest, Plugins};
pub use profiles::{
    AccountIdentity, AccountLinkInteraction, Profile, ProfileError, Profiles, ProfilesConfig,
    StorefrontAccount, StorefrontProvider,
};
pub use utils::directories::Directories;
pub use utils::env_vars::EnvVars;

pub(crate) use next_proto::winebridge as proto;
pub(crate) use utils::context::Context;
