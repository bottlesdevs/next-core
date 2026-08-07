mod edit;
pub(crate) mod error;
mod manager;
mod snapshot;
mod software;
mod state;

#[cfg(test)]
mod tests;

pub use crate::proto::{DllOverride, DllOverrideMode, Process, RegistryHive};
pub use crate::wrapper::{
    Wrappers,
    gamescope::{Filter as GamescopeFilter, GamescopeConfig, Scaler as GamescopeScaler},
    mangohud::MangoHudConfig,
};
pub use edit::BottleEdit;
pub use fvs_rs::{Commit as Snapshot, CommitSummary as SnapshotSummary};
pub use manager::BottleManager;
pub use state::{Bottle, BottleState, Program, Storage};
