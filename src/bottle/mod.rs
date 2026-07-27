mod delete;
mod edit;
pub(crate) mod error;
mod history;
mod manager;
pub(crate) mod prefix;
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
pub use delete::DeleteProgress;
pub use edit::BottleEdit;
pub use fvs_rs::{Commit as Snapshot, CommitSummary as SnapshotSummary};
pub use history::{RollbackProgress, SnapshotProgress};
pub use manager::{BottleManager, CreateProgress};
pub use prefix::{TransactionPhase, TransactionProgress, Transfer};
pub use software::{SetRunnerProgress, SetWinebridgeProgress};
pub use state::{Bottle, BottleState, BottleType, Program, RunnerSelection};

pub(crate) use prefix::{PrefixProgress, PrefixStorage};

pub(super) const FVS_BLOCK_SIZE: u32 = 1024 * 1024;
