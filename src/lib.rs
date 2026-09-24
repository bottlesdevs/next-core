#![doc = include_str!("../README.md")]
#![deny(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]
#![warn(rustdoc::all)]

mod addons;
mod bottle;
mod command;
mod core;
mod environment;
pub mod error;
mod library;
mod manager;
mod operation;
mod profiles;
#[cfg(feature = "fvs")]
mod program;
#[cfg(feature = "fvs")]
pub use program::{Program, ProgramState};
mod program_spec;
pub use program_spec::ProgramSpec;
mod runner;
mod utils;
#[cfg(feature = "fvs")]
mod virgo;
mod winebridge;

pub use addons::{
    Addon, AddonError, Addons, CatalogEntry, CatalogError, Component, Dependency, InstallerError,
    Requirement, Slot,
};
pub use bottle::{Bottle, BottleData, BottleError, BottleState};
pub use command::wrappers::{
    GamescopeConfig, GamescopeFilter, GamescopeScaler, MangoHudConfig, Wrappers,
};
pub use core::{Bottles, Config};
pub use environment::{Edit, EnvironmentConfig, EnvironmentError, PrefixBackend, State};
#[cfg(feature = "fvs")]
pub use environment::{Snapshot, SnapshotSummary};
pub use library::{Library, LibraryEntry, LibraryItem, LibraryProvider};
pub use manager::Manager;
pub use operation::{Operation, Progress, Stage, Transfer};
pub use profiles::{
    AccountIdentity, AccountLink, AccountLinkInteraction, AccountProviderInfo, Profile,
    ProfileError, Profiles, ProfilesState,
};
pub use proto::{DllOverride, DllOverrideMode, Process, RegistryHive};
pub use utils::directories::Directories;
pub use utils::env_vars::EnvVars;

pub(crate) use next_proto::winebridge as proto;
pub(crate) use utils::context::Context;
