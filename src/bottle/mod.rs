#[allow(clippy::module_inception)]
mod bottle;
mod edit;
pub(crate) mod error;
mod manager;
pub(crate) mod prefix;
mod snapshot;

#[cfg(test)]
mod tests;

pub use crate::proto::{DllOverride, DllOverrideMode, Process, RegistryHive};
pub use crate::wrapper::{
    Wrappers,
    gamescope::{Filter as GamescopeFilter, GamescopeConfig, Scaler as GamescopeScaler},
    mangohud::MangoHudConfig,
};
pub use bottle::{Bottle, BottleComponents, BottleState, BottleType, DeleteProgress, Program};
pub use edit::BottleEdit;
pub use fvs_rs::{Commit as Snapshot, CommitSummary as SnapshotSummary};
pub use manager::{BottleManager, CreateProgress};
pub use prefix::{TransactionPhase, TransactionProgress, Transfer};
pub use snapshot::{RollbackProgress, SnapshotProgress};

pub(crate) use prefix::{PrefixProgress, PrefixStorage};

pub(super) const FVS_BLOCK_SIZE: u32 = 1024 * 1024;
