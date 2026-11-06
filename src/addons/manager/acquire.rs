//! Acquisition of immutable releases from catalogs and local archives.
//!
//! Downloads and extraction happen in staging. A committed release contains its
//! payload, runtime metadata, and any frozen prefix recipe.

use super::super::{
    Addon, AddonError, AddonRole, CatalogError, Component, Requirement, RoleMarker,
    catalog::{CatalogArtifact, Target},
    defaults::steps as recipe_steps,
    recipe::InstallResource,
};
use super::{Addons, download::download};
use crate::{
    Operation, Progress, Stage,
    error::{Error, Result},
    utils::{fs, paths},
};
use download_manager::manager::DownloadManager;
use futures_util::TryStreamExt;
use std::path::{Path, PathBuf};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

impl Addons {
    /// Acquires the catalog release `id` with role `R`, or returns the already
    /// acquired release.
    ///
    /// Use [`AddonRole`] as `R` to acquire a release of any role. Runtime tools
    /// and components require exactly one artifact matching the current platform.
    /// Components freeze the artifact's recipe, or the slot's default recipe when
    /// the artifact has none. Dependencies download every matching artifact in
    /// catalog order. Downloads are cancellable; checksum reads and extraction
    /// finish before cancellation is observed.
    ///
    /// # Errors
    ///
    /// Returns an error on cancellation, a missing or unsupported catalog entry,
    /// a release with another role, ambiguous artifacts, download, verification,
    /// extraction, or storage failure.
    pub fn fetch<R: RoleMarker>(&self, id: Uuid) -> Operation<Addon<R>> {
        let addons = self.clone();
        Operation::new(move |progress, cancellation| async move {
            if let Some(release) = addons.acquired::<R>(id, &cancellation).await? {
                return Ok(release);
            }
            let state = addons.state();
            let entry = state
                .component_catalog
                .iter()
                .chain(state.dependency_catalog.iter())
                .find_map(|catalog| catalog.entry(id))
                .ok_or(CatalogError::NotFound(id))?;
            let role = R::try_from(entry.role()).map_err(|_| AddonError::WrongRole(id))?;
            let target = Target::current().ok_or(CatalogError::Unsupported(id))?;
            let artifacts: Vec<_> = entry.artifacts_for_target(target).collect();
            if artifacts.is_empty() {
                return Err(CatalogError::Unsupported(id).into());
            }
            let is_dependency = matches!(entry.role(), AddonRole::Dependency(_));
            if !is_dependency && artifacts.len() != 1 {
                return Err(CatalogError::InvalidArchiveArtifactCount {
                    addon: id,
                    count: artifacts.len(),
                }
                .into());
            }
            let record = Addon::new(
                entry.id(),
                entry.name().into(),
                entry.version().into(),
                entry.requirements().to_vec(),
                resources(entry.role(), &artifacts),
                role,
            );
            fs::with_temp_dir(&paths::staging(), |stage| async move {
                let downloads = stage.join("downloads");
                async_fs::create_dir(&downloads).await?;
                for artifact in &artifacts {
                    download_artifact(
                        &addons.0.downloader,
                        artifact,
                        &downloads.join(artifact.file_name()),
                        &progress,
                        &cancellation,
                    )
                    .await?;
                }
                let prepared = if is_dependency {
                    let prepared = stage.join("release");
                    async_fs::create_dir(&prepared).await?;
                    async_fs::rename(downloads, prepared.join("payload")).await?;
                    prepared
                } else {
                    let archive = downloads.join(artifacts[0].file_name());
                    prepare_archive(&archive, &stage, &cancellation).await?
                };
                addons.commit(record, &prepared, &cancellation).await
            })
            .await
        })
    }

    async fn acquired<R: RoleMarker>(
        &self,
        id: Uuid,
        cancellation: &CancellationToken,
    ) -> Result<Option<Addon<R>>> {
        let _write = cancellation
            .run_until_cancelled(self.0.write.lock())
            .await
            .ok_or(Error::Cancelled)?;
        if cancellation.is_cancelled() {
            return Err(Error::Cancelled);
        }
        match self.state().releases.get(&id) {
            Some(release) => Ok(Some(
                release.clone().cast().ok_or(AddonError::WrongRole(id))?,
            )),
            None => Ok(None),
        }
    }

    /// Imports a release with role `role` from a local tar archive under a fresh UUID.
    ///
    /// The archive must contain one top-level directory. Components freeze the
    /// default recipe for their slot; other roles have no recipe. `requirements`
    /// are frozen into the release. The source archive is left unchanged.
    ///
    /// # Errors
    ///
    /// Returns an error on cancellation, extraction, an escaping payload link, or
    /// storage failure. Import only trusted archives.
    pub fn import<R: RoleMarker>(
        &self,
        path: impl AsRef<Path>,
        name: impl Into<String>,
        version: impl Into<String>,
        requirements: Vec<Requirement>,
        role: R,
    ) -> Operation<Addon<R>> {
        let resources = resources(role.into(), &[]);
        let record = Addon::new(
            Uuid::new_v4(),
            name.into(),
            version.into(),
            requirements,
            resources,
            role,
        );
        let source = path.as_ref().to_path_buf();
        let addons = self.clone();
        Operation::new(move |progress, cancellation| async move {
            progress.send_replace(Some(Progress::new(Stage::Preparing)));
            fs::with_temp_dir(&paths::staging(), |stage| async move {
                let prepared = prepare_archive(&source, &stage, &cancellation).await?;
                addons.commit(record, &prepared, &cancellation).await
            })
            .await
        })
    }
}

/// Freezes the installation resources of a release with `role` from `artifacts`.
///
/// Components use their single artifact's steps, or the slot's default recipe.
/// Dependencies pair each artifact with its steps. Runtime tools have none.
fn resources(role: AddonRole, artifacts: &[&CatalogArtifact]) -> Vec<InstallResource> {
    match role {
        AddonRole::Component(Component { slot }) => vec![InstallResource::new(
            "",
            artifacts
                .first()
                .and_then(|artifact| artifact.steps())
                .unwrap_or_else(|| recipe_steps(slot))
                .to_vec(),
        )],
        AddonRole::Dependency(_) => artifacts
            .iter()
            .map(|artifact| {
                InstallResource::new(
                    artifact.file_name(),
                    artifact.steps().unwrap_or_default().to_vec(),
                )
            })
            .collect(),
        AddonRole::Runner(_) | AddonRole::WineBridge(_) | AddonRole::Umu(_) => Vec::new(),
    }
}

async fn download_artifact(
    downloader: &DownloadManager,
    artifact: &CatalogArtifact,
    destination: &Path,
    progress: &watch::Sender<Option<Progress>>,
    cancellation: &CancellationToken,
) -> Result<()> {
    download(
        downloader,
        artifact.url().clone(),
        destination,
        cancellation,
        |transfer| {
            progress.send_replace(Some(Progress::transferring(
                Stage::Downloading {
                    file: artifact.file_name().into(),
                },
                transfer,
            )));
        },
    )
    .await?;
    progress.send_replace(Some(Progress::new(Stage::Verifying {
        file: artifact.file_name().into(),
    })));
    if cancellation.is_cancelled() {
        return Err(Error::Cancelled);
    }
    if !artifact.checksum().verify(destination).await? {
        return Err(AddonError::ChecksumMismatch(destination.to_path_buf()).into());
    }
    Ok(())
}

// Addon archives have one top-level directory, which becomes the payload.
async fn prepare_archive(
    archive: &Path,
    stage: &Path,
    cancellation: &CancellationToken,
) -> Result<PathBuf> {
    let extracted = stage.join("extracted");
    async_fs::create_dir(&extracted).await?;
    if cancellation.is_cancelled() {
        return Err(Error::Cancelled);
    }
    crate::utils::fs::archive::extract(archive, &extracted).await?;
    if cancellation.is_cancelled() {
        return Err(Error::Cancelled);
    }
    let mut entries = async_fs::read_dir(&extracted).await?;
    let Some(entry) = entries.try_next().await? else {
        return Err(AddonError::InvalidArchive.into());
    };
    if entries.try_next().await?.is_some() || !entry.file_type().await?.is_dir() {
        return Err(AddonError::InvalidArchive.into());
    }
    let source = entry.path();
    check_payload_links(&source, cancellation).await?;
    let prepared = stage.join("release");
    async_fs::create_dir(&prepared).await?;
    async_fs::rename(source, prepared.join("payload")).await?;
    Ok(prepared)
}

// Addon links must stay inside the payload tree after it leaves staging.
async fn check_payload_links(root: &Path, cancellation: &CancellationToken) -> Result<()> {
    let root = async_fs::canonicalize(root).await?;
    let mut pending = vec![root.clone()];
    while let Some(directory) = pending.pop() {
        let mut entries = async_fs::read_dir(directory).await?;
        while let Some(entry) = entries.try_next().await? {
            if cancellation.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let path = entry.path();
            let kind = entry.file_type().await?;
            if kind.is_dir() {
                pending.push(path);
            } else if kind.is_symlink() {
                let target = async_fs::read_link(&path).await?;
                crate::utils::fs::archive::safe_symlink_target(
                    path.strip_prefix(&root).unwrap(),
                    target,
                )?;
                if !async_fs::canonicalize(&path).await?.starts_with(&root) {
                    return Err(AddonError::InvalidPayload(path).into());
                }
            }
        }
    }
    Ok(())
}
