//! Acquisition of catalog releases into local storage.
//!
//! Downloads and extraction happen in staging. A committed release contains its
//! payload, runtime metadata, and any frozen prefix recipe.

use super::{Catalogs, download::download};
use crate::{
    Operation, Progress, Stage,
    addons::{
        Addon, AddonError, AddonRole, CatalogError, RoleMarker,
        catalog::{CatalogArtifact, Target},
        recipe::resources,
        store::prepare_archive,
    },
    error::{Error, Result},
    utils::fs,
};
use download_manager::manager::DownloadManager;
use std::path::Path;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

impl Catalogs {
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
        let catalogs = self.clone();
        Operation::new(move |progress, cancellation| async move {
            let addons = catalogs.0.addons.clone();
            if let Some(release) = addons.acquired::<R>(id, &cancellation).await? {
                return Ok(release);
            }
            let state = catalogs.state();
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
            fs::with_staging(|stage| async move {
                let downloads = stage.join("downloads");
                async_fs::create_dir(&downloads).await?;
                for artifact in &artifacts {
                    download_artifact(
                        &catalogs.0.downloader,
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
