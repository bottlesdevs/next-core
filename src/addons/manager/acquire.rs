//! Acquisition of immutable releases from catalogs and local archives.
//!
//! Downloads and extraction happen in staging. A committed release contains its
//! payload, runtime metadata, and any frozen prefix recipe.

use super::super::{
    Addon, AddonError, CatalogError, Component, Dependency, Requirement, Runner, Slot, Umu,
    WineBridge,
    catalog::{AddonKind, CatalogArtifact, CatalogEntry, Target},
    defaults::steps as recipe_steps,
    recipe::InstallResource,
};
use super::{Addons, AddonsState, StoredAddon, download::download};
use crate::{
    Operation, Progress, Stage,
    error::{Error, Result},
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
    /// Acquires a runner archive, or returns its already acquired release.
    ///
    /// # Errors
    ///
    /// Returns an error on cancellation, a missing or unsupported catalog entry,
    /// ambiguous artifacts, download, verification, extraction, or storage failure.
    pub fn fetch_runner(&self, id: Uuid) -> Operation<Arc<Addon<Runner>>> {
        self.fetch(id, AddonsState::runner_entry, runtime_record::<Runner>)
    }

    /// Acquires a `WineBridge` archive, or returns its already acquired release.
    ///
    /// # Errors
    ///
    /// Returns an error on cancellation, a missing or unsupported catalog entry,
    /// ambiguous artifacts, download, verification, extraction, or storage failure.
    pub fn fetch_winebridge(&self, id: Uuid) -> Operation<Arc<Addon<WineBridge>>> {
        self.fetch(
            id,
            AddonsState::winebridge_entry,
            runtime_record::<WineBridge>,
        )
    }

    /// Acquires an UMU archive, or returns its already acquired release.
    ///
    /// # Errors
    ///
    /// Returns an error on cancellation, a missing or unsupported catalog entry,
    /// ambiguous artifacts, download, verification, extraction, or storage failure.
    pub fn fetch_umu(&self, id: Uuid) -> Operation<Arc<Addon<Umu>>> {
        self.fetch(id, AddonsState::umu_entry, runtime_record::<Umu>)
    }

    /// Acquires a component archive and freezes its selected installation recipe.
    ///
    /// An already acquired release with this UUID is returned unchanged. Exactly
    /// one artifact must match the current platform. Downloads are cancellable;
    /// checksum reads and extraction finish before cancellation is observed.
    ///
    /// # Errors
    ///
    /// Returns an error on cancellation, a missing or unsupported catalog entry,
    /// ambiguous artifacts, download, verification, extraction, or storage failure.
    pub fn fetch_component(&self, id: Uuid) -> Operation<Arc<Addon<Component>>> {
        self.fetch(id, AddonsState::component_entry, component_record)
    }

    /// Acquires a dependency and freezes its selected artifact recipes.
    ///
    /// An already acquired release with this UUID is returned unchanged. Every
    /// artifact matching the current platform is downloaded in catalog order.
    /// Downloads are cancellable; an in-progress checksum read finishes before
    /// cancellation is observed.
    ///
    /// # Errors
    ///
    /// Returns an error on cancellation, a missing or unsupported catalog entry,
    /// download, verification, or storage failure.
    pub fn fetch_dependency(&self, id: Uuid) -> Operation<Arc<Addon<Dependency>>> {
        self.fetch(id, AddonsState::dependency_entry, dependency_record)
    }

    fn fetch<K: StoredAddon>(
        &self,
        id: Uuid,
        lookup: fn(&AddonsState, Uuid) -> Option<CatalogEntry>,
        record: fn(&CatalogEntry, &[&CatalogArtifact]) -> Addon<K>,
    ) -> Operation<Arc<Addon<K>>> {
        let addons = self.clone();
        Operation::new(move |progress, cancellation| async move {
            if let Some(release) = addons.acquired::<K>(id, &cancellation).await? {
                return Ok(release);
            }
            let entry = lookup(&addons.state(), id).ok_or(CatalogError::NotFound(id))?;
            let target = Target::current().ok_or(CatalogError::Unsupported(id))?;
            let artifacts: Vec<_> = entry.artifacts_for_target(target).collect();
            if artifacts.is_empty() {
                return Err(CatalogError::Unsupported(id).into());
            }
            let is_dependency = entry.kind() == AddonKind::Dependency;
            if !is_dependency && artifacts.len() != 1 {
                return Err(CatalogError::InvalidArchiveArtifactCount {
                    addon: id,
                    count: artifacts.len(),
                }
                .into());
            }
            let record = Arc::new(record(&entry, &artifacts));
            fs::with_temp_dir(&addons.0.directories.staging(), |stage| async move {
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

    async fn acquired<K: StoredAddon>(
        &self,
        id: Uuid,
        cancellation: &CancellationToken,
    ) -> Result<Option<Arc<Addon<K>>>> {
        let _write = cancellation
            .run_until_cancelled(self.0.write.lock())
            .await
            .ok_or(Error::Cancelled)?;
        if cancellation.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let state = self.state();
        match state.releases.get(&id) {
            Some(release) => K::get(release)
                .cloned()
                .map(Some)
                .ok_or(AddonError::Duplicate(id).into()),
            None => Ok(None),
        }
    }

    /// Imports a runner from a local tar archive under a fresh UUID.
    ///
    /// The archive must contain one top-level directory. Its runner layout is
    /// detected when the release is used. The source archive is left unchanged.
    ///
    /// # Errors
    ///
    /// Returns an error on cancellation, extraction, an escaping payload link, or
    /// storage failure. Import only trusted archives.
    pub fn import_runner(
        &self,
        path: impl AsRef<Path>,
        name: impl Into<String>,
        version: impl Into<String>,
    ) -> Operation<Arc<Addon<Runner>>> {
        self.import_runtime(path, name.into(), version.into())
    }

    /// Imports `WineBridge` from a local tar archive under a fresh UUID.
    ///
    /// # Errors
    ///
    /// Returns an error on cancellation, extraction, an escaping payload link, or
    /// storage failure. Import only trusted archives with one top-level directory.
    pub fn import_winebridge(
        &self,
        path: impl AsRef<Path>,
        name: impl Into<String>,
        version: impl Into<String>,
    ) -> Operation<Arc<Addon<WineBridge>>> {
        self.import_runtime(path, name.into(), version.into())
    }

    /// Imports UMU from a local tar archive under a fresh UUID.
    ///
    /// # Errors
    ///
    /// Returns an error on cancellation, extraction, an escaping payload link, or
    /// storage failure. Import only trusted archives with one top-level directory.
    pub fn import_umu(
        &self,
        path: impl AsRef<Path>,
        name: impl Into<String>,
        version: impl Into<String>,
    ) -> Operation<Arc<Addon<Umu>>> {
        self.import_runtime(path, name.into(), version.into())
    }

    fn import_runtime<K: StoredAddon + Default>(
        &self,
        path: impl AsRef<Path>,
        name: String,
        version: String,
    ) -> Operation<Arc<Addon<K>>> {
        self.import_archive(
            path,
            Addon::new(Uuid::new_v4(), name, version, Vec::new(), K::default()),
        )
    }

    /// Imports a component from a local tar archive under a fresh UUID.
    ///
    /// The archive must contain one top-level directory. Built-in requirements
    /// and the default recipe for `slot` are frozen into the release. The source
    /// archive is left unchanged.
    ///
    /// # Errors
    ///
    /// Returns an error on cancellation, extraction, an escaping payload link, or
    /// storage failure. Import only trusted archives.
    pub fn import_component(
        &self,
        path: impl AsRef<Path>,
        slot: Slot,
        name: impl Into<String>,
        version: impl Into<String>,
    ) -> Operation<Arc<Addon<Component>>> {
        let requirements = match slot {
            Slot::Nvapi => vec![Requirement::Slot(Slot::Dxvk)],
            _ => Vec::new(),
        };
        self.import_archive(
            path,
            Addon::new(
                Uuid::new_v4(),
                name.into(),
                version.into(),
                requirements,
                Component {
                    slot,
                    resources: vec![InstallResource::new("", recipe_steps(slot).to_vec())],
                },
            ),
        )
    }

    fn import_archive<K: StoredAddon>(
        &self,
        path: impl AsRef<Path>,
        record: Addon<K>,
    ) -> Operation<Arc<Addon<K>>> {
        let source = path.as_ref().to_path_buf();
        let addons = self.clone();
        Operation::new(move |progress, cancellation| async move {
            progress.send_replace(Some(Progress::new(Stage::Preparing)));
            fs::with_temp_dir(&addons.0.directories.staging(), |stage| async move {
                let prepared = prepare_archive(&source, &stage, &cancellation).await?;
                addons
                    .commit(Arc::new(record), &prepared, &cancellation)
                    .await
            })
            .await
        })
    }
}

fn runtime_record<K: Default>(entry: &CatalogEntry, _artifacts: &[&CatalogArtifact]) -> Addon<K> {
    Addon::new(
        entry.id(),
        entry.name().into(),
        entry.version().into(),
        entry.requirements().to_vec(),
        K::default(),
    )
}

fn component_record(entry: &CatalogEntry, artifacts: &[&CatalogArtifact]) -> Addon<Component> {
    let AddonKind::Component { slot } = entry.kind() else {
        unreachable!("component entry lookup only returns components")
    };
    Addon::new(
        entry.id(),
        entry.name().into(),
        entry.version().into(),
        entry.requirements().to_vec(),
        Component {
            slot,
            resources: vec![InstallResource::new(
                "",
                artifacts[0]
                    .steps()
                    .unwrap_or_else(|| recipe_steps(slot))
                    .to_vec(),
            )],
        },
    )
}

fn dependency_record(entry: &CatalogEntry, artifacts: &[&CatalogArtifact]) -> Addon<Dependency> {
    Addon::new(
        entry.id(),
        entry.name().into(),
        entry.version().into(),
        entry.requirements().to_vec(),
        Dependency {
            resources: artifacts
                .iter()
                .map(|artifact| {
                    InstallResource::new(
                        artifact.file_name(),
                        artifact.steps().unwrap_or_default().to_vec(),
                    )
                })
                .collect(),
        },
    )
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
