mod addons;
mod bottle;
mod core;
mod credentials;
mod environment;
pub mod error;
mod library;
mod operation;
mod profiles;
#[cfg(feature = "fvs")]
mod program;
#[cfg(feature = "fvs")]
pub use program::{Program, ProgramManager, ProgramState};
mod program_spec;
pub use program_spec::ProgramSpec;
mod runner;
mod utils;
#[cfg(feature = "fvs")]
mod virgo;
mod winebridge;
mod wrapper;

pub use addons::{
    Addon, AddonError, Addons, CatalogEntry, CatalogError, Component, Dependency, InstallerError,
    Requirement, Slot,
};
pub use bottle::{
    Bottle, BottleError, BottleManager, BottleState, DllOverride, DllOverrideMode, GamescopeConfig,
    GamescopeFilter, GamescopeScaler, MangoHudConfig, Process, RegistryHive, Wrappers,
};
pub use bottles_plugin_host::{PluginError, PluginInfo, PluginManifest, Plugins};
pub use core::{Bottles, Config};
pub use environment::{Edit, EnvironmentError, EnvironmentState, PrefixBackend};
#[cfg(feature = "fvs")]
pub use environment::{Snapshot, SnapshotSummary};
pub use library::{Library, LibraryItem, SearchEntry, SearchSource};
pub use operation::{Operation, Progress, Stage, Transfer};
pub use profiles::{
    AccountIdentity, AccountLinkInteraction, Profile, ProfileError, Profiles, ProfilesConfig,
    StorefrontAccount, StorefrontProvider, add_plugin_imports,
};
pub use utils::directories::Directories;
pub use utils::env_vars::EnvVars;

pub(crate) use next_proto::winebridge as proto;
pub(crate) use utils::context::Context;
