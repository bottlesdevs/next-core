mod bottle;
mod compatibility;
mod core;
pub mod error;
mod operation;
mod runner;
mod utils;
mod winebridge;
mod wrapper;

pub use bottle::{
    Bottle, BottleEdit, BottleManager, BottleState, BottleType, CreateProgress, DeleteProgress,
    DllOverride, DllOverrideMode, GamescopeConfig, GamescopeFilter, GamescopeScaler,
    MangoHudConfig, Process, Program, RegistryHive, RollbackProgress, RunnerSelection, Snapshot,
    SnapshotProgress, SnapshotSummary, TransactionPhase, TransactionProgress, Transfer, Wrappers,
};
pub use compatibility::{
    components::{Component, ComponentManager, catalog::ComponentKind},
    dependencies::{Dependency, DependencyManager},
    installer::{InstallProgress, UninstallProgress},
};
pub use core::Core;
pub use operation::Operation;
pub use runner::RunnerKind;
pub use utils::{directories::Directories as Paths, environment::Environment};

pub(crate) use next_proto::winebridge as proto;
pub(crate) use utils::{context::Context, directories::Directories};
