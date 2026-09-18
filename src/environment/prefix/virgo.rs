//! Virgo policy: select build inputs and translate frozen selections into ordered layers.

use crate::{
    Addon, AddonError, Component, Context, EnvironmentError, EnvironmentState, Progress, Slot,
    Stage,
    addons::{AddonFamily, InstallInputs, execute},
    environment::runtime,
    error::{Error, Result},
    virgo::{LayerStore, VirgoLayer},
};
use std::{path::Path, sync::Arc};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

/// Shared Virgo build and layer-selection policy for one core instance.
pub(crate) struct VirgoManager {
    cx: Context,
    pub(in crate::environment) layers: LayerStore,
}

impl VirgoManager {
    /// Construct without touching storage or starting FVS.
    pub(crate) fn new(cx: Context) -> Self {
        Self {
            layers: LayerStore::new(cx.directories().clone(), cx.fvs().clone()),
            cx,
        }
    }

    /// Build from complete frozen selections before the owner publishes configuration.
    /// Return the prepared layers for owner materialization. Cached effects remain
    /// usable after shared source payloads are removed; launch only loads them.
    pub(in crate::environment) async fn prepare_artifacts(
        &self,
        config: &EnvironmentState,
        progress: &watch::Sender<Option<Progress>>,
        cancellation: &CancellationToken,
    ) -> Result<(VirgoLayer, Vec<VirgoLayer>)> {
        if cancellation.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let base = self.prepare_base(cancellation).await?;
        let mut overlays = vec![self.prepare_adapter(config, &base, cancellation).await?];
        for addon in config.ordered_components() {
            overlays.push(
                self.prepare_addon(addon, &base, progress, cancellation)
                    .await?,
            );
        }
        for addon in &config.dependencies {
            overlays.push(
                self.prepare_addon(addon, &base, progress, cancellation)
                    .await?,
            );
        }
        if cancellation.is_cancelled() {
            return Err(Error::Cancelled);
        }
        Ok((base, overlays))
    }

    /// Load the pinned base, runner adapter, components, then dependencies.
    pub(in crate::environment) async fn composition(
        &self,
        config: &EnvironmentState,
    ) -> Result<(VirgoLayer, Vec<VirgoLayer>)> {
        let base = self.layers.require(Path::new("soda"), None).await?;
        let runner = config.runner().id();
        let adapter = Path::new("adapters").join(runner.to_string());
        let mut overlays = vec![self.layers.require(&adapter, Some(runner)).await?];
        for id in config
            .ordered_components()
            .map(Addon::id)
            .chain(config.dependencies.iter().map(Addon::id))
        {
            let addon = Path::new("addons").join(id.to_string());
            overlays.push(self.layers.require(&addon, Some(id)).await?);
        }
        Ok((base, overlays))
    }

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

    async fn prepare_adapter(
        &self,
        config: &EnvironmentState,
        base: &VirgoLayer,
        cancellation: &CancellationToken,
    ) -> Result<VirgoLayer> {
        let id = config.runner().id();
        let destination = Path::new("adapters").join(id.to_string());
        self.layers
            .get_or_build(&destination, Some(id), cancellation, || async {
                let runner = config
                    .runner()
                    .load_runner(self.cx.directories(), config.umu())
                    .await?;
                let workspace = self
                    .layers
                    .prepare_build(&destination, Some(base), cancellation)
                    .await?;
                let executed = if cancellation.is_cancelled() {
                    Err(Error::Cancelled)
                } else {
                    runner.wineboot(&workspace.prefix, "--init").await
                };
                runtime::stop(runner.as_ref(), &workspace.prefix).await?;
                self.layers
                    .finish_build(workspace, id, id.to_string(), executed, cancellation)
                    .await
            })
            .await
    }

    /// Reuse cached effects or execute the selected frozen recipe in an isolated build.
    async fn prepare_addon<K: AddonFamily>(
        &self,
        addon: &Addon<K>,
        base: &VirgoLayer,
        progress: &watch::Sender<Option<Progress>>,
        cancellation: &CancellationToken,
    ) -> Result<VirgoLayer> {
        let id = addon.id();
        let destination = Path::new("addons").join(id.to_string());
        self.layers
            .get_or_build(&destination, Some(id), cancellation, || async {
                let soda = self
                    .cx
                    .addons()
                    .component(base.id)
                    .ok_or(AddonError::NotFound(base.id))?;
                let runner = soda.load_runner(self.cx.directories(), None).await?;
                let winebridge = latest_component(
                    self.cx
                        .addons()
                        .components()
                        .into_iter()
                        .filter(|addon| addon.slot() == Slot::WineBridge),
                )
                .ok_or(EnvironmentError::ComponentNotInstalled(Slot::WineBridge))?
                .path(self.cx.directories());
                let workspace = self
                    .layers
                    .prepare_build(&destination, Some(base), cancellation)
                    .await?;
                let executed = execute(
                    InstallInputs {
                        prefix: &workspace.prefix,
                        staging: &self.cx.directories().staging(),
                        runner: runner.as_ref(),
                        winebridge: &winebridge,
                    },
                    &addon.path(self.cx.directories()),
                    addon.resources(),
                    false,
                    cancellation,
                    |_| {
                        progress.send_replace(Some(Progress::new(Stage::Configuring)));
                    },
                )
                .await;
                runtime::stop(runner.as_ref(), &workspace.prefix).await?;
                self.layers
                    .finish_build(workspace, id, id.to_string(), executed, cancellation)
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
