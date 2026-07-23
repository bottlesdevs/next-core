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
    #[error("bottle ID {actual} does not match directory ID {expected}")]
    IdMismatch { expected: Uuid, actual: Uuid },
    #[error("program name and executable must not be empty")]
    InvalidProgram,
    #[error("program {0} was not found")]
    ProgramNotFound(Uuid),
    #[error("component {0} is not installed")]
    ComponentNotInstalled(Uuid),
    #[error("component {0} is required and cannot be uninstalled")]
    ComponentNotUninstallable(Uuid),
    #[error("runner component is required")]
    RunnerComponentRequired,
    #[error("runner components must be installed with Bottle::install_runner")]
    RunnerRequiresExplicitInstall,
    #[error("WineBridge component is required")]
    WinebridgeComponentRequired,
    #[error("UMU component has the wrong kind")]
    InvalidUmuComponent,
    #[error("Wine runner must not use UMU")]
    WineRunnerWithUmu,
    #[error("Proton runner requires UMU")]
    ProtonRunnerWithoutUmu,
    #[error("component cannot be installed into a prefix")]
    InvalidPrefixComponent,
}

#[derive(Debug, Error)]
pub enum VirgoError {
    #[error("FVS repository {repository} has no commit {state}")]
    MissingCommit { repository: PathBuf, state: String },
    #[error("Virgo base exists but has no commits")]
    EmptyBase,
    #[error("refusing to initialize non-empty Virgo base at {0}")]
    DirtyBase(PathBuf),
    #[error("mountpoint is not empty: {0}")]
    DirtyMountpoint(PathBuf),
    #[error("cached Virgo layer was not found: {0}")]
    CachedLayerNotFound(PathBuf),
    #[error("failed to process Virgo registry data: {0}")]
    Registry(String),
}
