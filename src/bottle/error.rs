//! Bottle-specific errors exposed through the crate's top-level error type.

use thiserror::Error;
use uuid::Uuid;

/// Bottle-specific failures carried by [`crate::error::Error::Bottle`].
#[derive(Debug, Error)]
pub enum BottleError {
    /// No program is registered with the requested UUID.
    #[error("program {0} was not found")]
    ProgramNotFound(Uuid),
}
