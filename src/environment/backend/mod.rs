//! Prefix storage strategies.
//!
//! [`PrefixBackend`] records whether an environment owns a conventional,
//! directly modified Wine prefix or a Virgo workspace assembled from immutable
//! layers.

pub(super) mod standard;
#[cfg(feature = "fvs")]
pub(super) mod virgo;

use serde::{Deserialize, Serialize};

/// Selects how an environment stores and materializes its Wine prefix.
///
/// This choice is persisted with the owner and cannot be changed by an edit.
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Serialize)]
pub enum PrefixBackend {
    /// Stores installed software and user data together in one mutable prefix.
    Standard,
    /// Composes immutable FVS layers over a private writable upper directory.
    #[cfg(feature = "fvs")]
    Virgo,
}
