//! Profile-specific errors exposed through the crate's top-level error type.

use thiserror::Error;

/// Profile-specific failures carried by [`crate::error::Error::Profile`].
#[derive(Debug, Error)]
pub enum ProfileError {
    #[error("no profile with id {0}")]
    NotFound(String),
}
