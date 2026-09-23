//! Wine-prefix lifecycle and configuration.
//!
//! A [`BottleManager`] owns the bottles known to one [`crate::Bottles`]
//! context. Its [`Bottle`] handles are live, cloneable references to shared
//! state; [`BottleState`] values returned by those handles are immutable
//! snapshots that do not change when the bottle is edited or deleted.
//! [`Bottle::edit`] applies a callback to the latest state and publishes it after
//! validation, software application, and persistence. [`crate::ProgramSpec`] defines
//! programs registered through that callback.
//!
//! Bottle directories and their `state.toml` files are library-managed.
//! Manager queries read an in-memory registry rather than rescanning or
//! reloading externally modified files. Component and dependency records are pinned in each
//! persisted state until a bottle operation explicitly replaces them.
//!
//! With the default `fvs` feature, bottles support caller-visible snapshots.
//! Standard history is created on demand; Virgo checkpoints software edits.
//! Long-running mutations return lazy
//! [`crate::Operation`] values and serialize with edits, stopping, snapshots,
//! and deletion. WineBridge-backed control calls share that coordination.

mod edit;
pub(crate) mod error;
mod manager;
#[cfg(feature = "fvs")]
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
pub use error::BottleError;
pub use manager::BottleManager;
pub use state::{Bottle, BottleData, BottleState};
