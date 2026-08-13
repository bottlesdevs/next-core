//! Addon fetch transactions.

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
    /// Downloads, extracts, validates, and atomically publishes a component.
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

    /// Downloads every platform artifact and atomically publishes a dependency.
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

fn single_path_component(value: &str) -> bool {
    let mut components = Path::new(value).components();
    matches!(components.next(), Some(PathComponent::Normal(_))) && components.next().is_none()
}

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

async fn top_level_directory(root: &Path) -> Result<PathBuf> {
    let mut entries = async_fs::read_dir(root).await?;
    let Some(entry) = entries.next().await.transpose()? else {
        return Err(AddonError::InvalidComponentArchive.into());
    };
    if entries.next().await.transpose()?.is_some() || !entry.file_type().await?.is_dir() {
        return Err(AddonError::InvalidComponentArchive.into());
    }
    Ok(entry.path())
}
