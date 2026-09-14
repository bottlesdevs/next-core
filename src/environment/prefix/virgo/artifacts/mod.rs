//! Shared immutable bases, runner adapters, and UUID-only addon caches.

mod adapter;
mod build;
mod cache;
mod software;
pub(crate) use cache::VirgoLayer;

use super::VirgoError;
use crate::environment::prefix::FVS_BLOCK_SIZE;
use crate::{
    Addons, CatalogEntry, Component, Context, Directories, EnvironmentState, Progress, Slot,
    error::{Error, Result},
};
use std::path::PathBuf;
use tokio::sync::{Mutex, watch};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// Shared artifact storage and construction for one core instance.
pub(crate) struct VirgoManager {
    cx: Context,
    addons: Addons,
    build_lock: Mutex<()>,
}

/// A resolved base followed by adapter and addon effects, shared by registry and mount assembly.
pub(super) struct VirgoComposition {
    pub(super) base: VirgoLayer,
    pub(super) overlays: Vec<VirgoLayer>,
}

impl VirgoManager {
    /// Construct without touching storage or starting FVS.
    pub(crate) fn new(cx: Context, addons: Addons) -> Self {
        Self {
            cx,
            addons,
            build_lock: Mutex::new(()),
        }
    }

    /// Ensure selected effects exist before the owner publishes its configuration.
    pub(in crate::environment) async fn apply(
        &self,
        config: &EnvironmentState,
        progress: &watch::Sender<Option<Progress>>,
        cancellation: &CancellationToken,
    ) -> Result<()> {
        if cancellation.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let base = self.prepare_base(cancellation).await?;
        self.prepare_adapter(config, &base, cancellation).await?;
        for id in addon_ids(config) {
            if cancellation.is_cancelled() {
                return Err(Error::Cancelled);
            }
            self.prepare_addon(id, &base, progress, cancellation)
                .await?;
        }
        if cancellation.is_cancelled() {
            return Err(Error::Cancelled);
        }
        Ok(())
    }

    fn staging_path(&self) -> PathBuf {
        let data_dir = self.cx.directories().data_dir();
        data_dir
            .join("virgo/.staging")
            .join(Uuid::new_v4().to_string())
    }
}

/// Load published artifacts
pub(super) async fn load(
    config: &EnvironmentState,
    directories: &Directories,
) -> Result<VirgoComposition> {
    let root = directories.data_dir().join("virgo");
    let base = cache::require(&root.join("soda"), None).await?;
    let runner = config.runner().id();
    let adapter = root.join("adapters").join(runner.to_string());
    let mut overlays = vec![cache::require(&adapter, Some(runner)).await?];
    for id in addon_ids(config) {
        let addon = root.join("addons").join(id.to_string());
        overlays.push(cache::require(&addon, Some(id)).await?);
    }
    Ok(VirgoComposition { base, overlays })
}

fn addon_ids(config: &EnvironmentState) -> impl Iterator<Item = Uuid> + '_ {
    config
        .ordered_components()
        .map(crate::Addon::id)
        .chain(config.dependencies.iter().map(crate::Addon::id))
}

fn latest_soda(entries: &[CatalogEntry<Component>]) -> Result<&CatalogEntry<Component>> {
    let mut latest = None;
    for entry in entries
        .iter()
        .filter(|entry| entry.slot() == Slot::Runner && entry.name().eq_ignore_ascii_case("soda"))
    {
        let version = semver::Version::parse(entry.version())
            .map_err(|_| VirgoError::InvalidSodaVersion(entry.version().into()))?;
        if latest
            .as_ref()
            .is_none_or(|(current, _)| &version > current)
        {
            latest = Some((version, entry));
        }
    }
    latest
        .map(|(_, entry)| entry)
        .ok_or_else(|| VirgoError::SodaNotInCatalog.into())
}

impl VirgoManager {
    /// Resolve the pinned Soda base, creating it under the shared build lock if absent.
    async fn prepare_base(&self, cancellation: &CancellationToken) -> Result<VirgoLayer> {
        let destination = self.cx.directories().data_dir().join("virgo/soda");
        if let Some(base) = cache::load(&destination, None).await? {
            return Ok(base);
        }
        let _build = cancellation
            .run_until_cancelled(self.build_lock.lock())
            .await
            .ok_or(Error::Cancelled)?;
        if let Some(base) = cache::load(&destination, None).await? {
            return Ok(base);
        }
        if cancellation.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let entries = self.addons.component_entries();
        let selected = latest_soda(&entries)?;
        let soda =
            self.addons
                .component(selected.id())
                .ok_or_else(|| VirgoError::SodaNotDownloaded {
                    id: selected.id(),
                    version: selected.version().into(),
                })?;
        let runner = soda
            .addon()
            .load_runner(self.cx.directories(), None)
            .await?;
        let stage = self.staging_path();
        let artifact = stage.join("artifact");
        let prefix = artifact.join("filesystem");
        let registry = artifact.join("registry");
        async_fs::create_dir_all(&prefix).await?;
        let initialized = runner.wineboot(&prefix, "--init").await;
        // Keep storage if Wine cannot be stopped safely.
        crate::environment::runtime::stop(runner.as_ref(), &prefix).await?;
        let result = async {
            initialized?;
            if cancellation.is_cancelled() {
                return Err(Error::Cancelled);
            }
            // Both copies describe this same stopped prefix, before atomic publication.
            super::registry::capture(&prefix, &registry).await?;
            let client = self.cx.fvs().await?;
            let repository = client.new_repository(&prefix, FVS_BLOCK_SIZE).await?;
            let commit = client
                .commit(
                    &repository,
                    format!("Soda {} ({})", soda.version(), soda.id()),
                )
                .await?;
            cache::publish(
                &artifact,
                &destination,
                soda.id(),
                commit.state_id,
                cancellation,
            )
            .await
        }
        .await;
        remove_dir(stage).await;
        result
    }
}

async fn remove_dir(path: PathBuf) {
    let _ = async_fs::remove_dir_all(path).await;
}
