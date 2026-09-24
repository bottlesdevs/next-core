//! Selects, builds, and orders immutable Virgo artifacts for an environment.
//!
//! Frozen addon selections resolve to a base layer, a runner adapter, component
//! layers, and dependency layers in composition order.

use crate::{
    Addon, AddonError, Component, Context, EnvironmentConfig, EnvironmentError, Progress, Slot,
    Stage,
    addons::{AddonFamily, InstallInputs, execute},
    environment::runtime,
    error::{Error, Result},
    virgo::{LayerStore, VirgoLayer},
};
use std::{path::Path, sync::Arc};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

pub(crate) struct VirgoManager {
    cx: Context,
    pub(in crate::environment) layers: LayerStore,
}

impl VirgoManager {
    /// Creates a manager without touching storage or connecting to FVS.
    pub(crate) fn new(cx: Context) -> Self {
        Self {
            layers: LayerStore::new(cx.directories().clone(), cx.fvs().clone()),
            cx,
        }
    }

    /// Ensures every artifact needed by `config` exists and returns composition order.
    ///
    /// Cached artifacts remain usable after their source payloads are removed.
    ///
    /// # Errors
    ///
    /// Returns cancellation, catalog, runner, installer, FVS, registry, or
    /// filesystem errors encountered while resolving or building an artifact.
    pub(in crate::environment) async fn prepare_artifacts(
        &self,
        config: &EnvironmentConfig,
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

    /// Loads the exact cached composition recorded by `config`.
    ///
    /// Layers are ordered as base, runner adapter, components in slot order, and
    /// dependencies in installation order.
    ///
    /// # Errors
    ///
    /// Returns [`crate::virgo::VirgoError::MissingArtifact`] for an absent cache
    /// entry, or an error when a manifest is unreadable or invalid.
    pub(in crate::environment) async fn composition(
        &self,
        config: &EnvironmentConfig,
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

    /// Reuses the pinned base or builds it from the newest valid local Soda runner.
    ///
    /// Semantic version determines recency and UUID breaks version ties. Missing
    /// inputs fail without downloading or selecting a different runner family.
    ///
    /// # Errors
    ///
    /// Returns [`EnvironmentError::SodaNotDownloaded`] when no eligible local
    /// runner exists, plus cancellation, runner, FVS, registry, or filesystem errors.
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

    /// Reuses or builds the selected runner's initialization delta over the base.
    ///
    /// # Errors
    ///
    /// Returns cancellation, runner loading, Wine initialization, FVS, registry,
    /// filesystem, or publication errors.
    async fn prepare_adapter(
        &self,
        config: &EnvironmentConfig,
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

    /// Reuses cached addon effects or executes the frozen recipe in an isolated build.
    ///
    /// # Errors
    ///
    /// Returns errors for missing build inputs, cancellation, runner loading,
    /// recipe execution, FVS, registry processing, filesystem work, or publication.
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

/// Selects the greatest valid semantic version, using UUID as a stable tie-breaker.
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
