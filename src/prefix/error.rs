//! Errors raised while loading, validating, or mutating prefixes.

use crate::{Requirement, Slot};
use thiserror::Error;
use uuid::Uuid;

/// Describes invalid prefix state and operations rejected by its lifecycle.
///
/// I/O, runner, addon, `WineBridge`, and Virgo failures are represented by other
/// variants of [`crate::error::Error`].
#[derive(Debug, Error)]
pub enum PrefixError {
    /// The collection has no prefix with the requested identifier.
    #[error("prefix {0} was not found")]
    NotFound(Uuid),
    /// The prefix was deleted while a handle to it remained alive.
    #[error("prefix was deleted")]
    Deleted,
    /// Restoring the automatic checkpoint after a failed mutation also failed.
    #[error("rollback failed for {root}; repair or restore this prefix before retrying: {source}")]
    Rollback {
        /// Root directory of the prefix that could not be recovered.
        root: std::path::PathBuf,
        /// Error returned while restoring the checkpoint.
        #[source]
        source: Box<crate::error::Error>,
    },

    /// Software selections cannot be changed while the prefix is running.
    #[error("stop the prefix before changing its settings")]
    MustBeStopped,
    /// A proposed edit violates an invariant not represented by another variant.
    #[error("invalid prefix edit: {0}")]
    InvalidEdit(&'static str),
    /// No selected component occupies the requested [`Slot`].
    #[error("component slot {0:?} is not installed")]
    ComponentNotInstalled(Slot),
    /// One or more addon requirements are absent from the proposed configuration.
    #[error("addon requirements are not satisfied: {requirements:?}")]
    RequiresAddon {
        /// Addon requesting the requirements.
        required_by: Uuid,
        /// Requirements not satisfied by any selected addon.
        requirements: Vec<Requirement>,
    },
    /// A component was stored under a [`Slot`] different from the one it declares.
    #[error("component {component} must occupy slot {required:?}")]
    InvalidComponentSlot {
        /// Identifier of the component stored in the wrong slot.
        component: Uuid,
        /// Slot declared by the component.
        required: Slot,
    },
}
