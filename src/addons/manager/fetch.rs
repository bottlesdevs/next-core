//! Fetch and publish complete immutable releases.

use super::super::{
    Addon, AddonError, CatalogError, Component, Dependency,
    catalog::{CatalogArtifact, Target},
    recipe::InstallResource,
    recipes::steps as recipe_steps,
};
use super::{Addons, download, prepare_component_archive};
use crate::{
    Operation, Progress, Stage,
    error::{Error, Result},
    utils::fs,
};
use download_manager::manager::DownloadManager;
use std::{path::Path, sync::Arc};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

impl Addons {
    /// Returns an existing local component or downloads the selected catalog release.
    /// Catalog refreshes never replace a local release's metadata or recipe.
    pub fn fetch_component(&self, id: Uuid) -> Operation<Arc<Addon<Component>>> {
        let addons = self.clone();
        Operation::new(move |progress, cancellation| async move {
            {
                let _write = cancellation
                    .run_until_cancelled(addons.0.write.lock())
                    .await
                    .ok_or(Error::Cancelled)?;
                if cancellation.is_cancelled() {
                    return Err(Error::Cancelled);
                }
                if let Some(release) = addons.component(id) {
                    return Ok(release);
                }
                if addons.state().contains(id) {
                    return Err(AddonError::Duplicate(id).into());
                }
            }
            let entry = addons
                .component_entry(id)
                .ok_or(CatalogError::NotFound(id))?;
            let target = Target::current().ok_or(CatalogError::Unsupported(id))?;
            let artifacts: Vec<_> = entry.artifacts_for_target(target).collect();
            if artifacts.is_empty() {
                return Err(CatalogError::Unsupported(id).into());
            }
            if artifacts.len() != 1 {
                return Err(CatalogError::InvalidComponentArtifactCount {
                    addon: id,
                    count: artifacts.len(),
                }
                .into());
            }
            let artifact = artifacts[0];
            let record = Arc::new(Addon::new_component(
                id,
                entry.name().into(),
                entry.version().into(),
                entry.slot(),
                entry.requirements().to_vec(),
                InstallResource::new(
                    "",
                    artifact
                        .steps()
                        .unwrap_or_else(|| recipe_steps(entry.slot()))
                        .to_vec(),
                ),
            ));
            fs::with_temp_dir(&addons.0.directories.staging(), |stage| async move {
                let downloads = stage.join("downloads");
                async_fs::create_dir(&downloads).await?;
                let file = downloads.join(artifact.file_name());
                download_artifact(
                    &addons.0.downloader,
                    artifact,
                    &file,
                    &progress,
                    &cancellation,
                )
                .await?;
                let prepared = prepare_component_archive(&file, &stage, &cancellation).await?;
                addons
                    .commit_component(record, &prepared, &cancellation)
                    .await
            })
            .await
        })
    }

    /// Returns an existing local dependency or downloads the selected catalog release.
    pub fn fetch_dependency(&self, id: Uuid) -> Operation<Arc<Addon<Dependency>>> {
        let addons = self.clone();
        Operation::new(move |progress, cancellation| async move {
            {
                let _write = cancellation
                    .run_until_cancelled(addons.0.write.lock())
                    .await
                    .ok_or(Error::Cancelled)?;
                if cancellation.is_cancelled() {
                    return Err(Error::Cancelled);
                }
                if let Some(release) = addons.dependency(id) {
                    return Ok(release);
                }
                if addons.state().contains(id) {
                    return Err(AddonError::Duplicate(id).into());
                }
            }
            let entry = addons
                .dependency_entry(id)
                .ok_or(CatalogError::NotFound(id))?;
            let target = Target::current().ok_or(CatalogError::Unsupported(id))?;
            let artifacts: Vec<_> = entry.artifacts_for_target(target).collect();
            if artifacts.is_empty() {
                return Err(CatalogError::Unsupported(id).into());
            }
            let record = Arc::new(Addon::new_dependency(
                id,
                entry.name().into(),
                entry.version().into(),
                entry.requirements().to_vec(),
                artifacts
                    .iter()
                    .map(|a| {
                        InstallResource::new(a.file_name(), a.steps().unwrap_or_default().to_vec())
                    })
                    .collect(),
            ));
            fs::with_temp_dir(&addons.0.directories.staging(), |stage| async move {
                let prepared = stage.join("release");
                let payload = prepared.join("payload");
                async_fs::create_dir_all(&payload).await?;
                for artifact in &artifacts {
                    download_artifact(
                        &addons.0.downloader,
                        artifact,
                        &payload.join(artifact.file_name()),
                        &progress,
                        &cancellation,
                    )
                    .await?;
                }
                addons
                    .commit_dependency(record, &prepared, &cancellation)
                    .await
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
