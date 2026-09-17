//! Backend configuration and policy for initialization and software materialization.
//! Environment owns process coordination and storage release.

pub(super) mod standard;
#[cfg(feature = "fvs")]
mod virgo;
#[cfg(feature = "fvs")]
pub use crate::virgo::VirgoError;
#[cfg(feature = "fvs")]
pub(crate) use virgo::VirgoManager;

use serde::{Deserialize, Serialize};

/// Selects how a runnable Wine prefix is created and maintained.
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Serialize)]
pub enum PrefixBackend {
    /// Initialize and mutate a conventional prefix directly.
    /// Explicit snapshots may use FVS; ordinary mutations use direct writes.
    Standard,
    /// Build immutable artifacts and compose them with a private writable upper.
    /// Virgo is experimental and requires the configured FVS service.
    #[cfg(feature = "fvs")]
    Virgo,
}
