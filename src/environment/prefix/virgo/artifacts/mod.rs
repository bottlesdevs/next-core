//! Shared immutable bases, runner adapters, and UUID-only addon caches.

mod adapter;
mod software;
use crate::virgo::{LayerStore, VirgoLayer};

use crate::{
    Addon, Component, Context, EnvironmentError, EnvironmentState, Progress, Slot,
    environment::runtime,
    error::{Error, Result},
};
use std::{path::Path, sync::Arc};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// Shared artifact storage and construction for one core instance.
pub(crate) struct VirgoManager {
    cx: Context,
    pub(super) layers: LayerStore,
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
        self.layers
            .get_or_build(destination, None, cancellation, || async {
                let soda =
                    latest_component(self.cx.addons().components().into_iter().filter(|addon| {
                        addon.slot() == Slot::Runner && addon.name().eq_ignore_ascii_case("soda")
                    }))
                    .ok_or(EnvironmentError::SodaNotDownloaded)?;
                let runner = soda.load_runner(self.cx.directories(), None).await?;
                let workspace = self
                    .layers
                    .prepare_build(destination, None, cancellation)
                    .await?;
                let executed = if cancellation.is_cancelled() {
                    Err(Error::Cancelled)
                } else {
                    runner.wineboot(&workspace.prefix, "--init").await
                };
                runtime::stop(runner.as_ref(), &workspace.prefix).await?;
                self.layers
                    .finish_build(
                        workspace,
                        soda.id(),
                        format!("Soda {} ({})", soda.version(), soda.id()),
                        executed,
                        cancellation,
                    )
                    .await
            })
            .await
    }
}

/// Internal build policy only: version order followed by immutable UUID identity.
fn latest_component(
    addons: impl Iterator<Item = Arc<Addon<Component>>>,
) -> Option<Arc<Addon<Component>>> {
    addons
        .filter_map(|addon| {
            semver::Version::parse(addon.version())
                .ok()
                .map(|version| (version, addon))
        })
        .max_by(|(a, left), (b, right)| a.cmp(b).then_with(|| left.id().cmp(&right.id())))
        .map(|(_, addon)| addon)
}
