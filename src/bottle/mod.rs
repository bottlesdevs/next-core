#[allow(clippy::module_inception)]
mod bottle;
pub(crate) mod error;
mod manager;
mod snapshot;
mod virgo;

#[cfg(test)]
mod tests;

pub use crate::wrapper::{
    Wrappers,
    gamescope::{Filter as GamescopeFilter, GamescopeConfig, Scaler as GamescopeScaler},
    mangohud::MangoHudConfig,
};
pub(crate) use bottle::PrefixStorage;
pub use bottle::{Bottle, BottleType, Program};
pub use fvs_rs::{Commit as Snapshot, CommitSummary as SnapshotSummary};
pub use manager::BottleManager;

pub(super) const FVS_BLOCK_SIZE: u32 = 1024 * 1024;
