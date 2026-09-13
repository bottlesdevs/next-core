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
    /// A program definition is malformed.
    #[error("invalid program: {0}")]
    InvalidProgram(String),
    /// No program is registered with the requested UUID.
    #[error("program {0} was not found")]
    ProgramNotFound(Uuid),
}
