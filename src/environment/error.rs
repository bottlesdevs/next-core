use crate::{Requirement, Slot};
use thiserror::Error;
use uuid::Uuid;

/// Failures in shared execution configuration and operations.
#[derive(Debug, Error)]
pub enum EnvironmentError {
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

    /// Cleanup could not finish; the prefix remains available for explicit shutdown.
    #[error(
        "cleanup failed at {prefix}; stop Wine and unmount this prefix before retrying: {source}"
    )]
    Cleanup {
        prefix: std::path::PathBuf,
        #[source]
        source: Box<crate::error::Error>,
    },
    #[error("stop the environment before changing its settings")]
    MustBeStopped,
    #[error("invalid environment edit: {0}")]
    InvalidEdit(&'static str),
    /// An environment variable name is empty or contains `=` or NUL.
    #[error(
        "invalid environment variable name {0:?}: names must be non-empty and contain neither '=' nor NUL"
    )]
    InvalidEnvironmentName(String),
    /// An environment variable value contains NUL.
    #[error("environment variable {0:?} contains NUL in its value")]
    InvalidEnvironmentValue(String),
    /// A DLL name is empty or contains NUL.
    ///
    /// This variant is reserved for local validation. The current DLL override
    /// methods delegate validation to WineBridge and return
    /// [`crate::error::Error::Status`] instead.
    #[error("DLL name {0:?} must be non-empty and contain no NUL bytes")]
    InvalidDllName(String),
    /// [`crate::DllOverrideMode::Unspecified`] was passed as an override mode.
    #[error("DLL override mode is required")]
    DllOverrideModeRequired,
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
