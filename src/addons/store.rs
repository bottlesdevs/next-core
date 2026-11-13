//! Locally acquired releases.
//!
//! [`Addons`] publishes immutable snapshots of the releases in local storage
//! behind a cheap cloneable handle. Imports, commits from
//! [`Catalogs::fetch`](crate::Catalogs::fetch), and removals replace the
//! snapshot.

use super::{
    Addon, AddonError, AddonRole, AddonSettingsSchema, Requirement, RoleMarker, Version,
    recipe::resources,
};
use crate::{
    Job, Operation, Stage,
    error::Result,
    utils::{fs, paths},
};
use futures_core::Stream;
use futures_util::TryStreamExt;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::sync::{Mutex, watch};
use tokio_stream::wrappers::WatchStream;
use uuid::Uuid;

impl next_config::Config for Addon<AddonRole> {
    const VERSION: u32 = 1;
}

/// Stores the releases that prefixes can select.
///
/// [`Addon`] values describe releases already stored locally. Acquiring a
/// release does not select it for any prefix. Cloned handles share the same
/// state. Read an immutable snapshot with [`state`](Self::state), or subscribe
/// with [`watch`](Self::watch) to observe later snapshots.
#[derive(Clone)]
pub struct Addons(Arc<AddonsInner>);

struct AddonsInner {
    published: watch::Sender<Arc<AddonsState>>,
    /// Serializes filesystem commits and state publication, not transfers.
    write: Mutex<()>,
}

/// An immutable snapshot of locally acquired releases.
///
/// Obtained through [`Addons::state`] or [`Addons::watch`]. Later publications do
/// not change this snapshot, and retaining it does not keep the store alive.
#[derive(Clone, Debug, Default)]
pub struct AddonsState {
    releases: HashMap<Uuid, Addon<AddonRole>>,
}

impl Addons {
    /// Opens the store from local release manifests.
    ///
    /// Release manifests are loaded independently of payload contents, so a
    /// returned store can still contain a record whose payload was modified
    /// outside the store.
    ///
    /// # Errors
    ///
    /// Returns an error if a release manifest cannot be read or parsed, or a
    /// manifest UUID differs from its directory name.
    pub(crate) async fn load() -> Result<Self> {
        let mut state = AddonsState::default();
        for (id, path) in release_manifests(&paths::releases()).await? {
            let record: Addon<AddonRole> = next_config::load(&path).await?;
            if record.id() != id {
                return Err(AddonError::InvalidRelease(path).into());
            }
            state.releases.insert(id, record);
        }
        Ok(Self(Arc::new(AddonsInner {
            published: watch::channel(Arc::new(state)).0,
            write: Mutex::new(()),
        })))
    }

    /// Returns the current immutable snapshot.
    pub fn state(&self) -> Arc<AddonsState> {
        self.0.published.borrow().clone()
    }

    /// Returns a stream of immutable release snapshots.
    ///
    /// The first item is available immediately. Later items follow successful
    /// new acquisitions, imports, and removals. Reusing an already-acquired
    /// release does not publish. Slow consumers may observe several publications
    /// as one item. The stream ends after the last store handle is dropped,
    /// including those held by operations.
    pub fn watch(&self) -> impl Stream<Item = Arc<AddonsState>> + Send + 'static + use<> {
        WatchStream::new(self.0.published.subscribe())
    }

    /// Publishes the already committed local snapshot without filesystem discovery.
    fn publish(&self, state: AddonsState) {
        self.0.published.send_replace(Arc::new(state));
    }

    /// Imports a release with role `role` from a local tar archive under a fresh UUID.
    ///
    /// The archive must contain one top-level directory. Components freeze the
    /// default recipe and its effects for their slot; other roles have no recipe. `requirements`
    /// are frozen into the release. The source archive is left unchanged.
    ///
    /// # Errors
    ///
    /// Returns an error on cancellation, extraction, an escaping payload link, or
    /// storage failure. Import only trusted archives.
    #[tracing::instrument(skip_all, fields(path = %path.as_ref().display()))]
    pub fn import<R: RoleMarker>(
        &self,
        path: impl AsRef<Path>,
        name: impl Into<String>,
        version: impl Into<String>,
        requirements: Vec<Requirement>,
        role: R,
    ) -> Operation<Addon<R>> {
        let (resources, effects) = resources(role.into(), &[]);
        let record = Addon::new(
            Uuid::new_v4(),
            name.into(),
            Version::new(version),
            requirements,
            resources,
            role,
        )
        .with_settings(AddonSettingsSchema::default().with_default_effects(effects));
        let source = path.as_ref().to_path_buf();
        let addons = self.clone();
        Operation::new(move |job| async move {
            job.transition(Stage::Preparing)?;
            fs::with_staging(|stage| async move {
                let prepared = prepare_archive(&source, &stage, &job).await?;
                addons.commit(record, &prepared, &job).await
            })
            .await
        })
    }

    /// Removes an acquired release of any role from shared storage.
    ///
    /// Existing prefix selections retain their records but lose the payload.
    /// Built Virgo artifacts are unaffected. The release is moved to trash before
    /// the new state is published; trash cleanup is best effort.
    ///
    /// # Errors
    ///
    /// Returns [`AddonError::NotFound`] when the release is absent, or an I/O error
    /// if its storage directory cannot be moved to trash.
    #[tracing::instrument(skip(self))]
    pub async fn remove(&self, id: Uuid) -> Result<()> {
        let _write = self.0.write.lock().await;
        let mut next = self.state().as_ref().clone();
        let release = next.releases.get(&id).ok_or(AddonError::NotFound(id))?;
        fs::discard(&release.directory()).await?;
        next.releases.remove(&id);
        self.publish(next);
        tracing::info!("release removed");
        Ok(())
    }

    /// Returns the release `id` if it is already acquired with role `R`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Cancelled`] if cancelled while waiting for the store, or
    /// [`AddonError::WrongRole`] if the release has another role.
    pub(super) async fn acquired<R: RoleMarker>(
        &self,
        id: Uuid,
        job: &Job,
    ) -> Result<Option<Addon<R>>> {
        let _write = job.cancellable(self.0.write.lock()).await?;
        match self.state().releases.get(&id) {
            Some(release) => Ok(Some(
                release.clone().cast().ok_or(AddonError::WrongRole(id))?,
            )),
            None => Ok(None),
        }
    }

    /// Commits a prepared release and publishes the updated snapshot.
    pub(super) async fn commit<R: RoleMarker>(
        &self,
        record: Addon<R>,
        prepared: &Path,
        job: &Job,
    ) -> Result<Addon<R>> {
        let id = record.id();
        let destination = record.directory();
        let _write = job.cancellable(self.0.write.lock()).await?;
        let mut next = self.state().as_ref().clone();
        if let Some(current) = next.releases.get(&id) {
            let current = current.clone().cast().ok_or(AddonError::WrongRole(id))?;
            if current != record {
                return Err(AddonError::InvalidRelease(destination).into());
            }
            return Ok(current);
        }
        if fs::exists(&destination).await? {
            return Err(AddonError::TargetExists(destination).into());
        }
        let stored = record.clone().erase();
        next_config::save(prepared.join("release.toml"), &stored).await?;
        async_fs::rename(prepared, destination).await?;
        next.releases.insert(id, stored);
        self.publish(next);
        tracing::info!(%id, name = record.name(), version = %record.version(), "release stored");
        Ok(record)
    }
}

impl AddonsState {
    /// Returns all locally acquired releases with role `R` in unspecified order.
    ///
    /// Use [`AddonRole`] as `R` to list releases of every role.
    pub fn releases<R: RoleMarker>(&self) -> impl Iterator<Item = Addon<R>> + '_ {
        self.releases
            .values()
            .filter_map(|release| release.clone().cast())
    }

    /// Returns the locally acquired release with UUID `id` if it has role `R`.
    pub fn release<R: RoleMarker>(&self, id: Uuid) -> Option<Addon<R>> {
        self.releases.get(&id)?.clone().cast()
    }
}

/// Collects expected manifest paths from UUID-named directories in `root`.
///
/// Non-directory entries and directories without UUID names are ignored.
///
/// # Errors
///
/// Returns an error if the release root or an entry's metadata cannot be read.
async fn release_manifests(root: &Path) -> Result<Vec<(Uuid, PathBuf)>> {
    let mut manifests = Vec::new();
    let mut entries = async_fs::read_dir(root).await?;
    while let Some(entry) = entries.try_next().await? {
        if !entry.file_type().await?.is_dir() {
            continue;
        }
        if let Ok(id) = Uuid::parse_str(&entry.file_name().to_string_lossy()) {
            manifests.push((id, entry.path().join("release.toml")));
        }
    }
    Ok(manifests)
}

// Addon archives have one top-level directory, which becomes the payload.
pub(super) async fn prepare_archive(archive: &Path, stage: &Path, job: &Job) -> Result<PathBuf> {
    let extracted = stage.join("extracted");
    async_fs::create_dir(&extracted).await?;
    job.check_cancellation()?;
    fs::archive::extract(archive, &extracted).await?;
    let mut entries = async_fs::read_dir(&extracted).await?;
    let Some(entry) = entries.try_next().await? else {
        return Err(AddonError::InvalidArchive.into());
    };
    if entries.try_next().await?.is_some() || !entry.file_type().await?.is_dir() {
        return Err(AddonError::InvalidArchive.into());
    }
    let source = entry.path();
    check_payload_links(&source).await?;
    let prepared = stage.join("release");
    async_fs::create_dir(&prepared).await?;
    async_fs::rename(source, prepared.join("payload")).await?;
    Ok(prepared)
}

// Addon links must stay inside the payload tree after it leaves staging.
async fn check_payload_links(root: &Path) -> Result<()> {
    let root = async_fs::canonicalize(root).await?;
    let mut pending = vec![root.clone()];
    while let Some(directory) = pending.pop() {
        let mut entries = async_fs::read_dir(directory).await?;
        while let Some(entry) = entries.try_next().await? {
            let path = entry.path();
            let kind = entry.file_type().await?;
            if kind.is_dir() {
                pending.push(path);
            } else if kind.is_symlink() {
                let target = async_fs::read_link(&path).await?;
                fs::archive::safe_symlink_target(path.strip_prefix(&root).unwrap(), target)?;
                if !async_fs::canonicalize(&path).await?.starts_with(&root) {
                    return Err(AddonError::InvalidPayload(path).into());
                }
            }
        }
    }
    Ok(())
}
