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
    MangoHudConfig, Process, Program, RegistryHive, RollbackProgress, RunnerSelection,
    SetRunnerProgress, Snapshot, SnapshotProgress, SnapshotSummary, Wrappers,
};
pub use compatibility::{
    CatalogKind, ComponentStatus, DependencyStatus, Library, LibraryError, LibraryProgress,
    LibraryState,
    components::{
        Component,
        catalog::{CatalogComponentEntry, ComponentArtifact, ComponentCatalog, ComponentKind},
    },
    dependencies::{
        Dependency,
        catalog::{CatalogDependencyEntry, DependencyCatalog, DependencyResource},
    },
    installer::{InstallProgress, UninstallProgress},
};
pub use core::Core;
pub use operation::Operation;
pub use prefix::{TransactionPhase, TransactionProgress, Transfer};
pub use runner::RunnerKind;
pub use utils::{directories::Directories as Paths, environment::Environment};

pub(crate) use next_proto::winebridge as proto;
pub(crate) use utils::{context::Context, directories::Directories};
