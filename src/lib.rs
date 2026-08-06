mod bottle;
mod compatibility;
mod core;
pub mod error;
mod operation;
mod prefix;
mod runner;
mod utils;
mod winebridge;
mod wrapper;

pub use bottle::{
    Bottle, BottleEdit, BottleManager, BottleState, BottleType, DllOverride, DllOverrideMode,
    GamescopeConfig, GamescopeFilter, GamescopeScaler, MangoHudConfig, Process, Program,
    RegistryHive, Snapshot, SnapshotSummary, Wrappers,
};
pub use compatibility::{
    Addon, Architecture, Availability, CatalogKind, Checksum, Library, LibraryError,
    OperatingSystem, RunnerComponent, Slot, Target,
};
pub use core::{Bottles, Config};
pub use operation::{Operation, Progress, Stage, Transfer};
pub use runner::RunnerKind;
pub use utils::environment::Environment;

pub(crate) use next_proto::winebridge as proto;
pub(crate) use utils::{context::Context, directories::Directories};
