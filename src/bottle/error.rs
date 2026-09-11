//! Bottle-specific errors exposed through the crate's top-level error type.

use thiserror::Error;
use uuid::Uuid;

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
    /// A program definition is malformed.
    #[error("invalid program: {0}")]
    InvalidProgram(String),
    /// No program is registered with the requested UUID.
    #[error("program {0} was not found")]
    ProgramNotFound(Uuid),
}
