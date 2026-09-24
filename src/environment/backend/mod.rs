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
/// Without the `fvs` feature, the `Virgo` variant is absent and persisted Virgo
/// owners cannot be deserialized.
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Serialize)]
pub enum PrefixBackend {
    /// Stores installed software and user data together in one mutable prefix.
    Standard,
    /// Composes immutable FVS layers over a private writable upper directory.
    ///
    /// Available only with the `fvs` crate feature.
    #[cfg(feature = "fvs")]
    Virgo,
}
