//! Addon-specific errors exposed through the crate's top-level error type.

use std::{path::PathBuf, process::ExitStatus};

use thiserror::Error;
use uuid::Uuid;

use crate::utils::archive::ArchiveError;

/// Addon-specific failures carried by [`crate::error::Error::Addon`].
#[derive(Debug, Error)]
pub enum AddonError {
    /// A catalog could not be loaded, validated, or refreshed.
    #[error(transparent)]
    Catalog(#[from] CatalogError),
    /// An installation recipe could not be executed safely or successfully.
    #[error(transparent)]
    Installer(#[from] InstallerError),
    /// An addon archive could not be read or safely extracted.
    #[error(transparent)]
    Archive(#[from] ArchiveError),
    /// An addon download could not be started or completed.
    #[error(transparent)]
    Download(#[from] download_manager::error::Error),
    /// The requested release is unknown or not present on disk for removal.
    #[error("addon {0} was not found")]
    NotFound(Uuid),
    /// Local storage contains more than one addon with the same immutable identifier.
    #[error("local storage contains duplicate addon {0}")]
    Duplicate(Uuid),
    /// A downloaded file did not match its catalog checksum.
    #[error("checksum mismatch for {0}")]
    ChecksumMismatch(PathBuf),
    /// A component archive did not contain exactly one top-level directory.
    #[error("an extracted artifact must contain exactly one top-level directory")]
    InvalidComponentArchive,
    /// A component directory lacks the marker required by its slot.
    #[error("component could not be identified: {0}")]
    InvalidComponent(PathBuf),
    /// A local addon index contains inconsistent or unsafe metadata.
    #[error("addon index is invalid: {0}")]
    InvalidAddonIndex(PathBuf),
    /// A component download would overwrite an existing version directory.
    #[error("addon target already exists: {0}")]
    TargetExists(PathBuf),
}

/// Failures caused by catalog configuration, contents, or compatibility.
#[derive(Debug, Error)]
pub enum CatalogError {
    /// The requested release is absent from the current catalog.
    #[error("catalog addon {0} was not found")]
    NotFound(Uuid),
    /// One or both catalogs failed to refresh after any successful catalog was published.
    #[error("catalog refresh failed (components: {components:?}, dependencies: {dependencies:?})")]
    Refresh {
        /// The component-catalog failure, or `None` when it refreshed successfully.
        components: Option<String>,
        /// The dependency-catalog failure, or `None` when it refreshed successfully.
        dependencies: Option<String>,
    },
    /// No URL was configured for one of the catalogs.
    #[error("{0} catalog URL is not configured")]
    UrlNotConfigured(&'static str),
    /// No catalog artifact supports this platform.
    #[error("no artifact supports this system for addon {0}")]
    Unsupported(Uuid),
    /// More than one artifact matched a component release on this platform.
    #[error("component {addon} has {count} matching artifacts; expected exactly one")]
    InvalidComponentArtifactCount {
        /// The component release containing the ambiguous artifacts.
        addon: Uuid,
        /// The number of artifacts matching the current platform.
        count: usize,
    },
    /// A catalog entry contains a path that is unsafe for managed storage.
    #[error("catalog entry contains an invalid storage path: {0}")]
    InvalidEntry(Uuid),
}

/// Failures caused by executing an addon's installation recipe.
#[derive(Debug, Error)]
pub enum InstallerError {
    /// A recipe-provided executable returned an unsuccessful exit status.
    #[error("installer exited with status {0}")]
    InstallerFailed(ExitStatus),
    /// Registering a DLL with `regsvr32` returned an unsuccessful exit status.
    #[error("regsvr32 exited with status {0}")]
    RegisterDllFailed(ExitStatus),
    /// An extracted file resolved outside its staging directory.
    #[error("staged file {path} is outside staging directory {stage}")]
    FileOutsideStage {
        /// The extracted path that escaped the staging directory.
        path: PathBuf,
        /// The staging directory that should have contained the path.
        stage: PathBuf,
    },
}
