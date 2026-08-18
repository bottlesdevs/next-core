//! Download, validation, and publication of catalog releases.

use std::{
    path::{Component as PathComponent, Path, PathBuf},
    sync::Arc,
};

use download_manager::manager::DownloadManager;
use futures_lite::StreamExt;
use futures_util::FutureExt;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use uuid::{NonNilUuid, Uuid};

use crate::{
    Operation, Progress, Stage,
    error::{Error, Result},
    utils::{archive, checksum, exists},
};

use super::super::{
    AddonError, CatalogError, Component, Dependency, IndexEntry,
    catalog::{CatalogArtifact, Target},
    index::AddonIndex,
    installer::Artifact,
};
use super::{Addons, download};

impl Addons {
    /// Downloads and publishes a component from the current catalog.
    ///
    /// If the catalog still contains `id` and that release is already indexed,
    /// the operation returns the existing entry without downloading it again.
    /// Otherwise, exactly one artifact must match the current platform. Its
    /// checksum, archive shape, slot-specific files, and storage paths are
    /// validated before the release is moved into shared storage and published.
    /// Fetching does not select the component in any bottle.
    ///
    /// Downloads and extraction occur outside the manager's write lock. The
    /// operation rechecks the index before committing, so concurrent fetches of
    /// the same release converge on the first published entry. Staging cleanup
    /// and rollback after a failed commit are best effort.
    ///
    /// # Errors
    ///
    /// The operation returns [`CatalogError::NotFound`] if `id` is absent from
    /// the current catalog, [`CatalogError::Unsupported`] if no artifact matches,
    /// or [`CatalogError::InvalidComponentArtifactCount`] if more than one
    /// matches. Invalid paths, checksum or archive failures, an occupied target,
    /// I/O and persistence failures, and cancellation are also returned.
    pub fn fetch_component(&self, id: Uuid) -> Operation<Arc<IndexEntry<Component>>> {
        let addons = self.clone();
        Operation::new(move |progress, cancellation| async move {
            let entry = addons
                .component_entry(id)
                .ok_or(CatalogError::NotFound(id))?;
            if let Some(component) = addons.component(id) {
                return Ok(component);
            }
            let target = Target::current().ok_or(CatalogError::Unsupported(entry.id()))?;
            let artifacts = entry.artifacts_for_target(target).collect::<Vec<_>>();
            if artifacts.is_empty() {
                return Err(CatalogError::Unsupported(entry.id()).into());
            }
            if artifacts.len() != 1 {
                return Err(CatalogError::InvalidComponentArtifactCount {
                    addon: entry.id(),
                    count: artifacts.len(),
                }
                .into());
            }
            let artifact = artifacts[0];
            if !single_path_component(entry.version())
                || !single_path_component(artifact.file_name())
            {
                return Err(CatalogError::InvalidEntry(entry.id()).into());
            }

            let stage = addons.create_stage().await?;
            let result = async {
                let file = stage.join(artifact.file_name());
                download_artifact(
                    addons.0.context.downloader(),
                    artifact,
                    &file,
                    progress,
                    &cancellation,
                )
                .await?;
                let extracted = stage.join("extracted");
                async_fs::create_dir_all(&extracted).await?;
                let extraction = archive::extract(&file, &extracted).fuse();
                let cancelled = cancellation.cancelled().fuse();
                futures_util::pin_mut!(extraction, cancelled);
                futures_util::select_biased! {
                    result = extraction => result?,
                    _ = cancelled => return Err(Error::Cancelled),
                }
                let release = top_level_directory(&extracted).await?;
                let slot = entry.slot();
                let requirements = AddonIndex::<Component>::inspect_release(slot, &release).await?;
                let _write = addons.0.write.lock().await;
                if cancellation.is_cancelled() {
                    return Err(Error::Cancelled);
                }
                if let Some(component) = addons.component(entry.id()) {
                    return Ok(component);
                }
                let state = addons.state();
                let target = AddonIndex::<Component>::target(
                    addons.0.context.directories(),
                    slot,
                    entry.version(),
                )
                .await?;
                if exists(&target).await? {
                    return Err(AddonError::TargetExists(target).into());
                }
                let component = IndexEntry::new_component(
                    NonNilUuid::new(entry.id()).expect("catalog UUID is non-nil"),
                    entry.name().to_owned(),
                    entry.version().to_owned(),
                    slot,
                    requirements,
                );
                let mut next = state.components.clone();
                next.addons.insert(component.id(), Arc::new(component));
                next.save(addons.0.context.directories()).await?;
                if let Err(error) = async_fs::rename(release, &target).await {
                    let _ = state.components.save(addons.0.context.directories()).await;
                    return Err(error.into());
                }
                let published = addons
                    .publish(
                        state.components.catalog.clone(),
                        state.dependencies.catalog.clone(),
                    )
                    .await
                    .and_then(|_| {
                        addons
                            .component(entry.id())
                            .ok_or_else(|| AddonError::NotFound(entry.id()).into())
                    });
                if published.is_err() {
                    let _ = async_fs::remove_dir_all(target).await;
                    let _ = state.components.save(addons.0.context.directories()).await;
                }
                published
            }
            .await;
            let _ = async_fs::remove_dir_all(stage).await;
            result
        })
    }

    /// Downloads and publishes a dependency from the current catalog.
    ///
    /// If the catalog still contains `id` and that release is already indexed,
    /// the operation returns the existing entry without downloading it again.
    /// Otherwise, every artifact matching the current platform is downloaded and
    /// checksum-verified. Their catalog recipes are retained in the index for
    /// later bottle installation. Fetching does not install the dependency into
    /// any bottle.
    ///
    /// Downloads occur outside the manager's write lock. The operation rechecks
    /// the index before committing, so concurrent fetches of the same release
    /// converge on the first published entry. Staging cleanup and rollback after
    /// a failed commit are best effort.
    ///
    /// # Errors
    ///
    /// The operation returns [`CatalogError::NotFound`] if `id` is absent from
    /// the current catalog or [`CatalogError::Unsupported`] if no artifact
    /// matches. Invalid paths, checksum failures, I/O and persistence failures,
    /// and cancellation are also returned.
    pub fn fetch_dependency(&self, id: Uuid) -> Operation<Arc<IndexEntry<Dependency>>> {
        let addons = self.clone();
        Operation::new(move |progress, cancellation| async move {
            let entry = addons
                .dependency_entry(id)
                .ok_or(CatalogError::NotFound(id))?;
            if let Some(dependency) = addons.dependency(id) {
                return Ok(dependency);
            }
            let target = Target::current().ok_or(CatalogError::Unsupported(entry.id()))?;
            let artifacts = entry.artifacts_for_target(target).collect::<Vec<_>>();
            if artifacts.is_empty() {
                return Err(CatalogError::Unsupported(entry.id()).into());
            }
            if artifacts
                .iter()
                .any(|artifact| !single_path_component(artifact.file_name()))
            {
                return Err(CatalogError::InvalidEntry(entry.id()).into());
            }

            let stage = addons.create_stage().await?;
            let result = async {
                for artifact in artifacts.iter().copied() {
                    download_artifact(
                        addons.0.context.downloader(),
                        artifact,
                        &stage.join(artifact.file_name()),
                        progress.clone(),
                        &cancellation,
                    )
                    .await?;
                }

                let _write = addons.0.write.lock().await;
                if cancellation.is_cancelled() {
                    return Err(Error::Cancelled);
                }
                if let Some(dependency) = addons.dependency(entry.id()) {
                    return Ok(dependency);
                }
                let state = addons.state();
                let target =
                    AddonIndex::<Dependency>::target(addons.0.context.directories(), entry.id())
                        .await?;
                if exists(&target).await? {
                    async_fs::remove_dir_all(&target).await?;
                }
                let dependency = IndexEntry::new_dependency(
                    NonNilUuid::new(entry.id()).expect("catalog UUID is non-nil"),
                    entry.name().to_owned(),
                    entry.version().to_owned(),
                    entry.requirements().to_vec(),
                    artifacts
                        .iter()
                        .map(|artifact| {
                            Artifact::new(
                                PathBuf::from(artifact.file_name()),
                                artifact.steps().to_vec(),
                            )
                        })
                        .collect(),
                );
                let mut next = state.dependencies.clone();
                next.addons.insert(dependency.id(), Arc::new(dependency));
                next.save(addons.0.context.directories()).await?;
                if let Err(error) = async_fs::rename(&stage, &target).await {
                    let _ = state
                        .dependencies
                        .save(addons.0.context.directories())
                        .await;
                    return Err(error.into());
                }
                let published = addons
                    .publish(
                        state.components.catalog.clone(),
                        state.dependencies.catalog.clone(),
                    )
                    .await
                    .and_then(|_| {
                        addons
                            .dependency(entry.id())
                            .ok_or_else(|| AddonError::NotFound(entry.id()).into())
                    });
                if published.is_err() {
                    let _ = async_fs::remove_dir_all(target).await;
                    let _ = state
                        .dependencies
                        .save(addons.0.context.directories())
                        .await;
                }
                published
            }
            .await;
            let _ = async_fs::remove_dir_all(stage).await;
            result
        })
    }
}

/// Restricts catalog-controlled names to one normal path component.
fn single_path_component(value: &str) -> bool {
    let mut components = Path::new(value).components();
    matches!(components.next(), Some(PathComponent::Normal(_))) && components.next().is_none()
}

/// Downloads one artifact and verifies its checksum before it can be committed.
async fn download_artifact(
    downloader: &DownloadManager,
    artifact: &CatalogArtifact,
    destination: &Path,
    progress: watch::Sender<Option<Progress>>,
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
                    file: artifact.file_name().to_owned(),
                },
                transfer,
            )));
        },
    )
    .await?;
    progress.send_replace(Some(Progress::new(Stage::Verifying {
        file: artifact.file_name().to_owned(),
    })));
    if !checksum::verify(destination, artifact.checksum()).await? {
        return Err(AddonError::ChecksumMismatch(destination.to_path_buf()).into());
    }
    if cancellation.is_cancelled() {
        return Err(Error::Cancelled);
    }
    Ok(())
}

/// Requires a component archive to contain exactly one top-level directory.
async fn top_level_directory(root: &Path) -> Result<PathBuf> {
    let mut entries = async_fs::read_dir(root).await?;

    let Some(entry) = entries.next().await.transpose()? else {
        return Err(AddonError::InvalidComponentArchive.into());
    };

    if entries.next().await.transpose()?.is_some() {
        return Err(AddonError::InvalidComponentArchive.into());
    }

    if entry.file_type().await?.is_dir() {
        Ok(entry.path())
    } else if entry.file_type().await?.is_file() {
        Ok(root.to_path_buf())
    } else {
        Err(AddonError::InvalidComponentArchive.into())
    }
}
