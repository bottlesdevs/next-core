use std::path::PathBuf;

use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum BottleError {
    #[error("application directories are unavailable on this platform")]
    ProjectDirectoriesUnavailable,
    #[error("a bottle named {0:?} already exists")]
    DuplicateName(String),
    #[error("bottle {0} was not found")]
    NotFound(Uuid),
    #[error("Virgo is unavailable because no fvs2d executable was configured")]
    VirgoUnavailable,
    #[error("program name and executable must not be empty")]
    InvalidProgram,
    #[error("program {0} was not found")]
    ProgramNotFound(Uuid),
    #[error("FVS repository {repository} has no commit {state}")]
    MissingCommit { repository: PathBuf, state: String },
    #[error("Virgo base exists but has no commits")]
    EmptyBase,
    #[error("refusing to initialize non-empty Virgo base at {0}")]
    DirtyBase(PathBuf),
    #[error("mountpoint is not empty: {0}")]
    DirtyMountpoint(PathBuf),
}
