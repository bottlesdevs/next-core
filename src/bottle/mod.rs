#[allow(clippy::module_inception)]
mod bottle;
pub(crate) mod error;
mod manager;
mod snapshot;
mod virgo;

#[cfg(test)]
mod tests;

pub use crate::proto::DllOverrideMode;
pub use crate::wrapper::{
    Wrappers,
    gamescope::{Filter as GamescopeFilter, GamescopeConfig, Scaler as GamescopeScaler},
    mangohud::MangoHudConfig,
};
pub use bottle::{Bottle, BottleComponents, BottleType, DllOverride, Program};
pub use fvs_rs::{Commit as Snapshot, CommitSummary as SnapshotSummary};
pub use manager::BottleManager;

pub(super) const FVS_BLOCK_SIZE: u32 = 1024 * 1024;
