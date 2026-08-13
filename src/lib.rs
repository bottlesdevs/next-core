mod addons;
mod bottle;
mod core;
pub mod error;
mod operation;
mod prefix;
mod runner;
mod utils;
mod winebridge;
mod wrapper;

pub use addons::{
    Addon, AddonError, AddonKind, Addons, CatalogEntry, CatalogError, Component, ComponentKind,
    Dependency, Dxvk, IndexEntry, InstallerError, LatencyFlex, Nvapi, Requirement, Runner, Slot,
    Umu, Vkd3d, WineBridge,
};
pub use bottle::{
    Bottle, BottleEdit, BottleManager, BottleState, DllOverride, DllOverrideMode, GamescopeConfig,
    GamescopeFilter, GamescopeScaler, MangoHudConfig, Process, Program, RegistryHive, Snapshot,
    SnapshotSummary, Storage, Wrappers,
};
pub use core::{Bottles, Config};
pub use operation::{Operation, Progress, Stage, Transfer};
pub use utils::environment::Environment;

pub(crate) use next_proto::winebridge as proto;
pub(crate) use utils::{context::Context, directories::Directories};
