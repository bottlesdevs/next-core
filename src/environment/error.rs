//! Errors raised while loading, validating, or mutating environments.

use crate::{Requirement, Slot};
use thiserror::Error;
use uuid::Uuid;

/// Describes invalid environment state and operations rejected by its lifecycle.
///
/// I/O, runner, addon, WineBridge, and Virgo failures are represented by other
/// variants of [`crate::error::Error`].
#[derive(Debug, Error)]
pub enum EnvironmentError {
    /// No downloaded Soda runner has a parseable semantic version.
    #[cfg(feature = "fvs")]
    #[error("no locally recorded Soda runner with a valid semantic version")]
    SodaNotDownloaded,
    /// The manager has no environment with the requested identifier.
    #[error("environment {0} was not found")]
    NotFound(Uuid),
    /// The environment was deleted while a handle to it remained alive.
    #[error("environment was deleted")]
    Deleted,
    /// Persisted state belongs to a different environment directory.
    #[error("environment ID {actual} does not match expected ID {expected}")]
    IdMismatch {
        /// Identifier encoded by the environment directory.
        expected: Uuid,
        /// Identifier stored in the loaded state.
        actual: Uuid,
    },
    /// Restoring the automatic checkpoint after a failed mutation also failed.
    #[cfg(feature = "fvs")]
    #[error("rollback failed for {root}; repair or restore this owner before retrying: {source}")]
    Rollback {
        /// Root directory of the environment that could not be recovered.
        root: std::path::PathBuf,
        /// Error returned while restoring the checkpoint.
        #[source]
        source: Box<crate::error::Error>,
    },

    /// Software selections cannot be changed while the environment is running.
    #[error("stop the environment before changing its settings")]
    MustBeStopped,
    /// A proposed edit violates an invariant not represented by another variant.
    #[error("invalid environment edit: {0}")]
    InvalidEdit(&'static str),
    /// No selected component occupies the requested [`Slot`].
    #[error("component slot {0:?} is not installed")]
    ComponentNotInstalled(Slot),
    /// One or more addon requirements are absent from the proposed configuration.
    #[error("addon requirements are not satisfied: {requirements:?}")]
    RequiresAddon {
        /// Addon requesting the requirements, or `None` when required runtime slots are absent.
        required_by: Option<Uuid>,
        /// Requirements not satisfied by any selected component or dependency.
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
