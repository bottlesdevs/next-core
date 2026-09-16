//! Shared immutable bases, runner adapters, and UUID-only addon caches.

mod adapter;
mod build;
mod software;
use crate::virgo::{FVS_BLOCK_SIZE, LayerStore, VirgoLayer, registry};

use crate::{
    Context, EnvironmentError, EnvironmentState, Progress, Slot,
    error::{Error, Result},
};
use std::path::{Path, PathBuf};
use tokio::sync::{Mutex, watch};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// Shared artifact storage and construction for one core instance.
pub(crate) struct VirgoManager {
    cx: Context,
    pub(super) layers: LayerStore,
    build_lock: Mutex<()>,
}

/// A resolved base followed by adapter and addon effects, shared by registry and mount assembly.
pub(super) struct VirgoComposition {
    pub(super) base: VirgoLayer,
    pub(super) overlays: Vec<VirgoLayer>,
}

impl VirgoManager {
    /// Construct without touching storage or starting FVS.
    pub(crate) fn new(cx: Context) -> Self {
        Self {
            layers: LayerStore::new(cx.directories().data_dir().join("virgo"), cx.fvs().clone()),
            cx,
            build_lock: Mutex::new(()),
        }
    }

    /// Build from complete frozen selections before the owner publishes configuration.
    /// Cached effects remain usable after shared source payloads are removed; startup
    /// only loads and composes these already published artifacts.
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
        for addon in config.ordered_components() {
            self.prepare_addon(addon, &base, progress, cancellation)
                .await?;
        }
        for addon in &config.dependencies {
            self.prepare_addon(addon, &base, progress, cancellation)
                .await?;
        }
        if cancellation.is_cancelled() {
            return Err(Error::Cancelled);
        }
        Ok(())
    }
}

/// Load published artifacts
pub(super) async fn load(
    config: &EnvironmentState,
    store: &LayerStore,
) -> Result<VirgoComposition> {
    let base = store.require(Path::new("soda"), None).await?;
    let runner = config.runner().id();
    let adapter = Path::new("adapters").join(runner.to_string());
    let mut overlays = vec![store.require(&adapter, Some(runner)).await?];
    for id in addon_ids(config) {
        let addon = Path::new("addons").join(id.to_string());
        overlays.push(store.require(&addon, Some(id)).await?);
    }
    Ok(VirgoComposition { base, overlays })
}

fn addon_ids(config: &EnvironmentState) -> impl Iterator<Item = Uuid> + '_ {
    config
        .ordered_components()
        .map(crate::Addon::id)
        .chain(config.dependencies.iter().map(crate::Addon::id))
}

impl VirgoManager {
    /// Reuse the pinned base, or build from the greatest valid local Soda version.
    /// UUID breaks version ties. Missing inputs fail without downloading or falling back.
    async fn prepare_base(&self, cancellation: &CancellationToken) -> Result<VirgoLayer> {
        let destination = Path::new("soda");
        if let Some(base) = self.layers.load(destination, None).await? {
            return Ok(base);
        }
        let _build = cancellation
            .run_until_cancelled(self.build_lock.lock())
            .await
            .ok_or(Error::Cancelled)?;
        if let Some(base) = self.layers.load(destination, None).await? {
            return Ok(base);
        }
        if cancellation.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let soda = self
            .cx
            .addons()
            .components()
            .into_iter()
            .filter(|addon| {
                addon.slot() == Slot::Runner && addon.name().eq_ignore_ascii_case("soda")
            })
            .filter_map(|addon| {
                semver::Version::parse(addon.version())
                    .ok()
                    .map(|version| (version, addon))
            })
            .max_by(|(a, left), (b, right)| a.cmp(b).then_with(|| left.id().cmp(&right.id())))
            .map(|(_, addon)| addon)
            .ok_or(EnvironmentError::SodaNotDownloaded)?;
        let runner = soda.load_runner(self.cx.directories(), None).await?;
        let stage = self.layers.staging_path();
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
            registry::capture(&prefix, &registry).await?;
            let client = self.cx.fvs();
            let repository = client.new_repository(&prefix, FVS_BLOCK_SIZE).await?;
            let commit = client
                .commit(
                    &repository,
                    format!("Soda {} ({})", soda.version(), soda.id()),
                )
                .await?;
            self.layers
                .publish(
                    &artifact,
                    destination,
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
