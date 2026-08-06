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
    Bottle, BottleEdit, BottleManager, BottleState, BottleType, CreateProgress, DeleteProgress,
    DllOverride, DllOverrideMode, GamescopeConfig, GamescopeFilter, GamescopeScaler,
    MangoHudConfig, Process, Program, RegistryHive, RollbackProgress, SetRunnerProgress, Snapshot,
    SnapshotProgress, SnapshotSummary, Wrappers,
};
pub use compatibility::{
    Addon, Architecture, Availability, CatalogKind, Checksum, Library, LibraryError,
    LibraryProgress, OperatingSystem, RunnerComponent, Slot, Target,
    installer::{InstallProgress, UninstallProgress},
};
pub use core::{Bottles, Config};
pub use operation::Operation;
pub use prefix::{TransactionPhase, TransactionProgress, Transfer};
pub use runner::RunnerKind;
pub use utils::environment::Environment;

pub(crate) use next_proto::winebridge as proto;
pub(crate) use utils::{context::Context, directories::Directories};
