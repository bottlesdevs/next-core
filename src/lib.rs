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
mod winebridge;

pub use addons::{
    Addon, AddonError, AddonRole, Addons, AddonsState, CatalogEntry, CatalogError, Component,
    Dependency, InstallerError, Requirement, RoleMarker, Runner, Slot, Umu, WineBridge,
};
pub use command::wrappers::{
    GamescopeConfig, GamescopeFilter, GamescopeScaler, MangoHudConfig, Wrappers,
};
pub use core::{Bottles, Config};
pub use operation::{Operation, Progress, Stage, Transfer};
pub use prefix::{Edit, Inventory, Prefix, PrefixBackend, PrefixError, PrefixState};
#[cfg(feature = "fvs")]
pub use prefix::{Snapshot, SnapshotSummary};
pub use prefixes::Prefixes;
pub use proto::{DllOverride, DllOverrideMode, Process, RegistryHive};
pub use utils::env_vars::EnvVars;

pub(crate) use next_proto::winebridge as proto;
pub(crate) use utils::context::Context;
