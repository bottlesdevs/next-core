//! Error types for catalog access, release acquisition, and recipe execution.

use std::{path::PathBuf, process::ExitStatus};

use thiserror::Error;
use uuid::Uuid;

use crate::utils::fs::archive::ArchiveError;

/// Reports a failure while managing, acquiring, or applying an addon.
///
/// Catalog and installer failures remain available through their source variants,
/// while filesystem, configuration, and cancellation failures may be returned by
/// the crate's top-level [`crate::error::Error`] directly.
#[derive(Debug, Error)]
pub enum AddonError {
    /// Wraps a catalog configuration, validation, or compatibility failure.
    #[error(transparent)]
    Catalog(#[from] CatalogError),
    /// Wraps a failure reported by an installation recipe.
    #[error(transparent)]
    Installer(#[from] InstallerError),
    /// Wraps archive validation or extraction failure.
    #[error(transparent)]
    Archive(#[from] ArchiveError),
    /// Wraps a download-manager failure.
    #[error(transparent)]
    Download(#[from] download_manager::error::Error),
    /// No locally acquired release has the requested UUID.
    #[error("addon {0} was not found")]
    NotFound(Uuid),
    /// The acquired record exists, but its shared payload is unavailable.
    #[error("source payload is missing for addon {0}")]
    PayloadMissing(Uuid),
    /// The UUID is already used by another local addon family or record.
    #[error("local storage contains duplicate addon {0}")]
    Duplicate(Uuid),
    /// A downloaded artifact did not match its catalog digest.
    #[error("checksum mismatch for {0}")]
    ChecksumMismatch(PathBuf),
    /// An imported or downloaded component archive has no single top-level directory.
    #[error("an extracted artifact must contain exactly one top-level directory")]
    InvalidComponentArchive,
    /// A component payload is structurally invalid or contains an escaping link.
    #[error("component could not be identified: {0}")]
    InvalidComponent(PathBuf),
    /// A persisted release record conflicts with its storage location or identity.
    #[error("release record is invalid: {0}")]
    InvalidRelease(PathBuf),
    /// Committing a release would overwrite an unmanaged filesystem entry.
    #[error("addon target already exists: {0}")]
    TargetExists(PathBuf),
}

/// Reports invalid catalog configuration, contents, or platform compatibility.
#[derive(Debug, Error)]
pub enum CatalogError {
    /// The requested UUID is absent from the currently published catalog.
    #[error("catalog addon {0} was not found")]
    NotFound(Uuid),
    /// At least one catalog failed during a two-family refresh.
    #[error("catalog refresh failed (components: {components:?}, dependencies: {dependencies:?})")]
    Refresh {
        /// Text of the component-catalog failure, or `None` after success.
        components: Option<String>,
        /// Text of the dependency-catalog failure, or `None` after success.
        dependencies: Option<String>,
    },
    /// No remote URL is configured for the named addon family.
    #[error("{0} catalog URL is not configured")]
    UrlNotConfigured(&'static str),
    /// The release has no artifact for the current build target.
    #[error("no artifact supports this system for addon {0}")]
    Unsupported(Uuid),
    /// A component has more than one artifact for the current build target.
    #[error("component {addon} has {count} matching artifacts; expected exactly one")]
    InvalidComponentArtifactCount {
        /// UUID of the component with ambiguous artifacts.
        addon: Uuid,
        /// Number of artifacts that matched the current target.
        count: usize,
    },
    /// A catalog entry contains a file name unsafe for managed storage.
    #[error("catalog entry contains an invalid storage path: {0}")]
    InvalidEntry(Uuid),
}

/// Reports a recipe step that could not be completed safely or successfully.
#[derive(Debug, Error)]
pub enum InstallerError {
    /// A recipe executable exited unsuccessfully.
    #[error("installer exited with status {0}")]
    InstallerFailed(ExitStatus),
    /// A `regsvr32` child for a DLL-registration step exited unsuccessfully.
    #[error("regsvr32 exited with status {0}")]
    RegisterDllFailed(ExitStatus),
    /// An extracted file resolved outside the temporary staging tree.
    #[error("staged file {path} is outside staging directory {stage}")]
    FileOutsideStage {
        /// Resolved file path outside the staging tree.
        path: PathBuf,
        /// Staging root that should have contained `path`.
        stage: PathBuf,
    },
}
