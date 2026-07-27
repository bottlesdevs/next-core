mod bottle;
mod compatibility;
pub mod error;
mod operation;
mod runner;
mod utils;
mod winebridge;
mod wrapper;

pub use bottle::{
    Bottle, BottleComponents, BottleEdit, BottleManager, BottleState, BottleType, CreateProgress,
    DeleteProgress, DllOverride, DllOverrideMode, GamescopeConfig, GamescopeFilter,
    GamescopeScaler, MangoHudConfig, Process, Program, RegistryHive, RollbackProgress, Snapshot,
    SnapshotProgress, SnapshotSummary, TransactionPhase, TransactionProgress, Transfer, Wrappers,
};
pub use compatibility::{
    components::{Component, ComponentManager, catalog::ComponentKind},
    dependencies::{Dependency, DependencyManager},
    installer::{InstallProgress, UninstallProgress},
};
pub use operation::Operation;
pub use runner::RunnerKind;
pub use utils::{context::Context, directories::Directories, environment::Environment};

pub(crate) use next_proto::winebridge as proto;
