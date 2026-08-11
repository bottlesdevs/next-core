//! Wine-prefix lifecycle and configuration.
//!
//! A [`BottleManager`] owns the bottles known to one [`crate::Bottles`]
//! context. Its [`Bottle`] handles are live, cloneable references to shared
//! state; [`BottleState`] values returned by those handles are immutable
//! snapshots that do not change when the bottle is edited or deleted.
//! Configuration changes are queued with [`Bottle::edit`] and become visible
//! only after [`BottleEdit::commit`] persists them.
//!
//! Bottle directories and their `bottle.toml` files are library-managed.
//! Manager queries read an in-memory registry rather than rescanning or
//! reloading externally modified files. Component and dependency records are pinned in each
//! persisted state until a bottle operation explicitly replaces them.
//!
//! Every bottle has an FVS repository, regardless of its [`Storage`] strategy.
//! FVS provides caller-visible snapshots and the internal checkpoints used to
//! recover from failed addon changes. Long-running mutations return lazy
//! [`crate::Operation`] values and serialize with edits, stopping, snapshots,
//! and deletion. WineBridge-backed requests may run concurrently.

mod edit;
pub(crate) mod error;
mod manager;
mod snapshot;
mod software;
mod state;

#[cfg(test)]
mod tests;

pub use crate::proto::DllOverride;
pub use crate::proto::DllOverrideMode;
pub use crate::proto::Process;
pub use crate::proto::RegistryHive;
pub use crate::wrapper::{
    Wrappers,
    gamescope::{Filter as GamescopeFilter, GamescopeConfig, Scaler as GamescopeScaler},
    mangohud::MangoHudConfig,
};
pub use edit::BottleEdit;
pub use fvs_rs::Commit as Snapshot;
pub use fvs_rs::CommitSummary as SnapshotSummary;
pub use manager::BottleManager;
pub use state::{Bottle, BottleState, Program, Storage};
