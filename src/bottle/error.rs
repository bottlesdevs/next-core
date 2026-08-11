//! Bottle-specific errors exposed through the crate's top-level error type.

use std::path::PathBuf;

use thiserror::Error;
use uuid::Uuid;

use crate::{Requirement, Slot};

/// Bottle-specific failures carried by [`crate::error::Error::Bottle`].
#[derive(Debug, Error)]
pub enum BottleError {
    /// No platform-specific application data directory could be determined.
    #[error("application directories are unavailable on this platform")]
    ProjectDirectoriesUnavailable,
    /// No persisted bottle exists for the requested UUID.
    #[error("bottle {0} was not found")]
    NotFound(Uuid),
    /// An operation used a handle after its bottle was deleted.
    #[error("bottle {0} was deleted")]
    Deleted(Uuid),
    /// Loaded or restored bottle metadata belongs to a different bottle.
    #[error("bottle ID {actual} does not match directory ID {expected}")]
    IdMismatch {
        /// UUID of the live handle or requested bottle.
        expected: Uuid,
        /// UUID found in the loaded metadata.
        actual: Uuid,
    },
    /// A program has a blank display name or executable.
    #[error("program name and executable must not be empty")]
    InvalidProgram,
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
    /// No program is registered with the requested UUID.
    #[error("program {0} was not found")]
    ProgramNotFound(Uuid),
    /// No selected component occupies the requested slot.
    #[error("component slot {0:?} is not installed")]
    ComponentNotInstalled(Slot),
    /// One or more dependencies must be downloaded or installed before the operation.
    #[error("addon requirements are not satisfied: {requirements:?}")]
    RequiresAddon {
        /// Release requesting the dependencies, or `None` for bottle creation.
        required_by: Option<Uuid>,
        /// Every currently unsatisfied requirement.
        requirements: Vec<Requirement>,
    },
    /// A bottle operation received a component for a different role.
    #[error("component {component} must occupy slot {required:?}")]
    InvalidComponentSlot { component: Uuid, required: Slot },
}

/// Virgo-specific failures carried by [`crate::error::Error::Virgo`].
#[derive(Debug, Error)]
pub enum VirgoError {
    /// A required FVS commit is missing from a repository.
    #[error("FVS repository {repository} has no commit {state}")]
    MissingCommit {
        /// Repository whose history was searched.
        repository: PathBuf,
        /// Requested full or abbreviated state ID.
        state: String,
    },
    /// An existing Virgo base repository has no commits to use as a layer.
    #[error("Virgo base exists but has no commits")]
    EmptyBase,
    /// Virgo cannot initialize a base over an existing nonempty directory.
    #[error("refusing to initialize non-empty Virgo base at {0}")]
    DirtyBase(PathBuf),
    /// Virgo cannot mount a prefix over a nonempty mountpoint.
    #[error("mountpoint is not empty: {0}")]
    DirtyMountpoint(PathBuf),
    /// A cached layer required to construct the prefix is missing.
    #[error("cached Virgo layer was not found: {0}")]
    CachedLayerNotFound(PathBuf),
    /// Registry data could not be converted while building a Virgo layer.
    #[error("failed to process Virgo registry data: {0}")]
    Registry(String),
}
