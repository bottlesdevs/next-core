//! Shared immutable bases, runner adapters, and UUID-only addon caches.

mod adapter;
mod build;
mod cache;
mod software;
pub(crate) use cache::VirgoLayer;

use super::VirgoError;
use crate::environment::prefix::FVS_BLOCK_SIZE;
use crate::{
    Addons, CatalogEntry, Component, Context, EnvironmentConfig, Progress, Slot,
    error::{Error, Result},
    runner::Runner,
};
use std::path::PathBuf;
use tokio::sync::{Mutex, watch};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// Shared artifact storage and construction for one core context.
pub(crate) struct VirgoManager {
    root: PathBuf,
    build_lock: Mutex<()>,
}

/// A resolved base followed by adapter and addon effects, shared by registry and mount assembly.
pub(super) struct VirgoComposition {
    pub(super) base: VirgoLayer,
    pub(super) overlays: Vec<VirgoLayer>,
}

impl VirgoManager {
    /// Construct without touching storage or starting FVS.
    pub(crate) fn new(root: PathBuf) -> Self {
        Self {
            root,
            build_lock: Mutex::new(()),
        }
    }

    /// Resolve shared effects without reading or mutating an owner's directory.
    pub(super) async fn resolve(
        &self,
        config: &EnvironmentConfig,
        runner: &dyn Runner,
        cx: &Context,
        addons: &Addons,
        progress: &watch::Sender<Option<Progress>>,
        cancellation: &CancellationToken,
    ) -> Result<VirgoComposition> {
        let base = self.prepare_base(addons, cx, cancellation).await?;
        let adapter = self
            .prepare_adapter(config.runner().id(), runner, &base, cx, cancellation)
            .await?;
        let mut overlays = vec![adapter];
        let ids = config
            .ordered_components()
            .map(crate::Addon::id)
            .chain(config.dependencies.iter().map(crate::Addon::id));
        for id in ids {
            overlays.push(
                self.prepare_addon(id, &base, addons, cx, progress, cancellation)
                    .await?,
            );
        }
        Ok(VirgoComposition { base, overlays })
    }

    fn staging_path(&self) -> PathBuf {
        self.root.join(".staging").join(Uuid::new_v4().to_string())
    }
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
    async fn prepare_base(
        &self,
        addons: &Addons,
        cx: &Context,
        cancellation: &CancellationToken,
    ) -> Result<VirgoLayer> {
        let destination = self.root.join("soda");
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
        let entries = addons.component_entries();
        let selected = latest_soda(&entries)?;
        let soda =
            addons
                .component(selected.id())
                .ok_or_else(|| VirgoError::SodaNotDownloaded {
                    id: selected.id(),
                    version: selected.version().into(),
                })?;
        let runner = soda.addon().load_runner(cx.directories(), None).await?;
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
            let client = cx.fvs().await?;
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
