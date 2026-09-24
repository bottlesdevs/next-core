//! Acquisition of immutable releases from catalogs and local archives.
//!
//! Downloaded artifacts are prepared in temporary storage, verified, and then
//! committed atomically to the family release directory. A committed release
//! includes both its payload and the recipe resolved during acquisition.

use super::super::{
    Addon, AddonError, CatalogError, Component, Dependency, Requirement, Slot,
    catalog::{CatalogArtifact, Target},
    defaults::steps as recipe_steps,
    recipe::InstallResource,
};
use super::{Addons, download::download};
use crate::{
    Operation, Progress, Stage,
    error::{Error, Result},
    runner::{RunnerKind, detect_runner_kind},
    utils::fs,
};
use download_manager::manager::DownloadManager;
use futures_util::TryStreamExt;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

impl Addons {
    /// Acquires the component identified by `id`.
    ///
    /// The returned [`Operation`] reuses the local component with this UUID when
    /// present, without comparing it with the current catalog entry.
    /// Otherwise it selects the single artifact for the current platform, verifies
    /// its checksum, extracts its one top-level directory, and stores the resulting
    /// immutable release. A later catalog refresh does not alter that stored record.
    /// Downloads are cancellable, but checksum reads and archive extraction finish
    /// before cancellation is observed.
    ///
    /// # Errors
    ///
    /// The operation fails if it is cancelled before commit; `id` is missing,
    /// unsupported, or collides with another addon; the component has anything
    /// other than one matching artifact; download or checksum verification fails;
    /// the archive is invalid; or the release cannot be committed to local storage.
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
                let state = addons.state();
                if let Some(release) = state.component(id) {
                    return Ok(release);
                }
                if state.contains(id) {
                    return Err(AddonError::Duplicate(id).into());
                }
            }
            let entry = addons
                .state()
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

    /// Acquires the dependency identified by `id`.
    ///
    /// The returned [`Operation`] reuses the local dependency with this UUID when
    /// present, without comparing it with the current catalog entry.
    /// Otherwise every artifact matching the current platform is downloaded,
    /// verified, and stored as the dependency payload in catalog order. Downloads
    /// are cancellable, but an in-progress checksum read finishes before cancellation
    /// is observed.
    ///
    /// # Errors
    ///
    /// The operation fails if it is cancelled before commit; `id` is missing,
    /// unsupported, or collides with another addon; a download or checksum
    /// verification fails; or the release cannot be committed to local storage.
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
                let state = addons.state();
                if let Some(release) = state.dependency(id) {
                    return Ok(release);
                }
                if state.contains(id) {
                    return Err(AddonError::Duplicate(id).into());
                }
            }
            let entry = addons
                .state()
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

    /// Imports a component from a local tar archive.
    ///
    /// Supported inputs are `.tar`, `.tar.gz`/`.tgz`, and `.tar.xz`/`.txz` files
    /// containing exactly one top-level directory. The returned [`Operation`]
    /// assigns a fresh UUID, infers built-in requirements for `slot`, freezes the
    /// slot's default recipe, and leaves the source archive unchanged.
    ///
    /// Extraction is not sandboxed. Path checks reject lexical traversal and
    /// escaping links, but cannot prevent a later archive entry from traversing a
    /// symlink created by an earlier entry; import only trusted archives.
    /// Extraction and runner inspection finish before cancellation is observed.
    ///
    /// # Errors
    ///
    /// The operation fails if it is cancelled before commit; the path is not a
    /// supported archive; extraction fails; the archive does not contain exactly
    /// one top-level directory; a symbolic link escapes that directory; the runner
    /// payload cannot be identified; or the new release cannot be committed to
    /// local storage.
    pub fn import_component(
        &self,
        path: impl AsRef<Path>,
        slot: Slot,
        name: impl Into<String>,
        version: impl Into<String>,
    ) -> Operation<Arc<Addon<Component>>> {
        let source = path.as_ref().to_path_buf();
        let name = name.into();
        let version = version.into();
        let addons = self.clone();
        Operation::new(move |progress, cancellation| async move {
            progress.send_replace(Some(Progress::new(Stage::Preparing)));
            fs::with_temp_dir(&addons.0.directories.staging(), |stage| async move {
                let prepared = prepare_component_archive(&source, &stage, &cancellation).await?;
                let payload = prepared.join("payload");
                let requirements = inspect_release(slot, &payload).await?;
                let steps = recipe_steps(slot).to_vec();
                let id = Uuid::new_v4();
                let release = Addon::new_component(
                    id,
                    name,
                    version,
                    slot,
                    requirements,
                    InstallResource::new("", steps),
                );
                addons
                    .commit_component(Arc::new(release), &prepared, &cancellation)
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

async fn inspect_release(slot: Slot, path: &Path) -> Result<Vec<Requirement>> {
    Ok(match slot {
        Slot::Runner if detect_runner_kind(path).await? == RunnerKind::Proton => {
            vec![Requirement::Slot(Slot::Umu)]
        }
        Slot::Nvapi => vec![Requirement::Slot(Slot::Dxvk)],
        _ => Vec::new(),
    })
}

// Component archives have one top-level directory, which becomes the payload.
async fn prepare_component_archive(
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
        return Err(AddonError::InvalidComponentArchive.into());
    };
    if entries.try_next().await?.is_some() || !entry.file_type().await?.is_dir() {
        return Err(AddonError::InvalidComponentArchive.into());
    }
    let source = entry.path();
    check_component_links(&source, cancellation).await?;
    let prepared = stage.join("release");
    async_fs::create_dir(&prepared).await?;
    async_fs::rename(source, prepared.join("payload")).await?;
    Ok(prepared)
}

// Component links must stay inside the component tree after it leaves staging.
async fn check_component_links(root: &Path, cancellation: &CancellationToken) -> Result<()> {
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
                    return Err(AddonError::InvalidComponent(path).into());
                }
            }
        }
    }
    Ok(())
}
