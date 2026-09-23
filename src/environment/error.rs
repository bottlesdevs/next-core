use crate::{Requirement, Slot};
use thiserror::Error;
use uuid::Uuid;

/// Failures in shared execution configuration and operations.
#[derive(Debug, Error)]
pub enum EnvironmentError {
    #[cfg(feature = "fvs")]
    #[error("no locally recorded Soda runner with a valid semantic version")]
    SodaNotDownloaded,
    #[error("environment {0} was not found")]
    NotFound(Uuid),
    #[error("environment was deleted")]
    Deleted,
    #[error("environment ID {actual} does not match expected ID {expected}")]
    IdMismatch { expected: Uuid, actual: Uuid },
    #[cfg(feature = "fvs")]
    #[error("rollback failed for {root}; repair or restore this owner before retrying: {source}")]
    Rollback {
        root: std::path::PathBuf,
        #[source]
        source: Box<crate::error::Error>,
    },

    #[error("stop the environment before changing its settings")]
    MustBeStopped,
    #[error("invalid environment edit: {0}")]
    InvalidEdit(&'static str),
    /// No selected component occupies the requested slot.
    #[error("component slot {0:?} is not installed")]
    ComponentNotInstalled(Slot),
    /// One or more dependencies must be downloaded or installed before the operation.
    #[error("addon requirements are not satisfied: {requirements:?}")]
    RequiresAddon {
        /// Release requesting the dependencies, or `None` for environment creation.
        required_by: Option<Uuid>,
        /// Every currently unsatisfied requirement.
        requirements: Vec<Requirement>,
    },
    /// An environment operation received a component for a different role.
    #[error("component {component} must occupy slot {required:?}")]
    InvalidComponentSlot { component: Uuid, required: Slot },
}
