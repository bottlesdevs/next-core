//! Selects, builds, and orders immutable Virgo artifacts for a prefix.
//!
//! Frozen addon selections resolve to a base layer, a runner adapter,
//! component layers, and dependency layers in composition order.

use crate::{
    Addon, AddonError, Context, Job, PrefixState,
    addons::{InstallInputs, execute},
    error::Result,
    prefix::runtime,
    virgo::{LayerStore, VirgoError, VirgoLayer},
};
use std::path::Path;

/// Resolves and builds Virgo artifacts with the services of one core instance.
pub(in crate::prefix) struct Virgo<'a> {
    cx: &'a Context,
    layers: &'a LayerStore,
}

impl<'a> Virgo<'a> {
    pub(in crate::prefix) fn new(cx: &'a Context) -> Self {
        Self {
            cx,
            layers: &cx.fvs().layers,
        }
    }

    /// Ensures every artifact needed by `config` exists and returns composition order.
    ///
    /// Cached artifacts remain usable after their source payloads are removed.
    /// Cancellation or a later build failure does not remove artifacts published
    /// earlier in the same pass. Failed process shutdown or an uncertain FVS
    /// request can retain build staging or a mount for diagnosis.
    ///
    /// # Errors
    ///
    /// Returns cancellation, catalog, runner, installer, FVS, registry, or
    /// filesystem errors encountered while resolving or building an artifact.
    pub(in crate::prefix) async fn prepare_artifacts(
        &self,
        config: &PrefixState,
        job: &Job,
    ) -> Result<(VirgoLayer, Vec<VirgoLayer>)> {
        let base = self.prepare_base(job).await?;
        let mut overlays = vec![self.prepare_adapter(config, &base, job).await?];
        for addon in config.components.values() {
            overlays.push(self.prepare_addon(addon, &base, job).await?);
        }
        for addon in &config.dependencies {
            overlays.push(self.prepare_addon(addon, &base, job).await?);
        }
        Ok((base, overlays))
    }

    /// Loads the exact cached composition recorded by `config`.
    ///
    /// Layers are ordered as base, runner adapter, components, and
    /// dependencies in installation order.
    ///
    /// # Errors
    ///
    /// Returns [`crate::virgo::VirgoError::MissingArtifact`] for an absent cache
    /// entry, or an error when a manifest is unreadable or invalid.
    pub(in crate::prefix) async fn composition(
        &self,
        config: &PrefixState,
    ) -> Result<(VirgoLayer, Vec<VirgoLayer>)> {
        let base = self.layers.require(Path::new("soda"), None).await?;
        let runner = config.runner.id();
        let adapter = Path::new("adapters").join(runner.to_string());
        let mut overlays = vec![self.layers.require(&adapter, Some(runner)).await?];
        for id in config
            .components
            .values()
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
    /// Returns [`VirgoError::SodaNotDownloaded`] when no eligible local
    /// runner exists, plus cancellation, runner, FVS, registry, or filesystem errors.
    async fn prepare_base(&self, job: &Job) -> Result<VirgoLayer> {
        let destination = Path::new("soda");
        self.layers
            .get_or_build(destination, None, job, || async {
                let soda = latest_addon(
                    self.cx
                        .addons()
                        .state()
                        .releases::<crate::Runner>()
                        .filter(|addon| addon.name().eq_ignore_ascii_case("soda")),
                )
                .ok_or(VirgoError::SodaNotDownloaded)?;
                let runner = soda.load_runner(None).await?;
                let workspace = self.layers.prepare_build(destination, None, job).await?;
                let executed = async {
                    job.check_cancellation()?;
                    runner.wineboot(&workspace.wineprefix, "--init").await
                }
                .await;
                runtime::stop(runner.as_ref(), &workspace.wineprefix).await?;
                self.layers
                    .finish_build(
                        workspace,
                        soda.id(),
                        format!("Soda {} ({})", soda.version(), soda.id()),
                        executed,
                        job,
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
        config: &PrefixState,
        base: &VirgoLayer,
        job: &Job,
    ) -> Result<VirgoLayer> {
        let id = config.runner.id();
        let destination = Path::new("adapters").join(id.to_string());
        self.layers
            .get_or_build(&destination, Some(id), job, || async {
                let runner = config.runner.load_runner(config.umu.as_ref()).await?;
                let workspace = self
                    .layers
                    .prepare_build(&destination, Some(base), job)
                    .await?;
                let executed = async {
                    job.check_cancellation()?;
                    runner.wineboot(&workspace.wineprefix, "--init").await
                }
                .await;
                runtime::stop(runner.as_ref(), &workspace.wineprefix).await?;
                self.layers
                    .finish_build(workspace, id, id.to_string(), executed, job)
                    .await
            })
            .await
    }

    /// Reuses cached addon effects or executes the frozen recipe in an isolated build.
    ///
    /// Cache misses execute with the pinned Soda base runner and the greatest
    /// semantic-versioned local `WineBridge`, independently of the prefix's
    /// selected runtime tools.
    ///
    /// # Errors
    ///
    /// Returns errors for missing build inputs, cancellation, runner loading,
    /// recipe execution, FVS, registry processing, filesystem work, or publication.
    async fn prepare_addon<R: crate::RoleMarker>(
        &self,
        addon: &Addon<R>,
        base: &VirgoLayer,
        job: &Job,
    ) -> Result<VirgoLayer> {
        let id = addon.id();
        let destination = Path::new("addons").join(id.to_string());
        self.layers
            .get_or_build(&destination, Some(id), job, || async {
                let addons = self.cx.addons().state();
                let soda = addons
                    .release::<crate::Runner>(base.id)
                    .ok_or(AddonError::NotFound(base.id))?;
                let runner = soda.load_runner(None).await?;
                let winebridge = latest_addon(addons.releases::<crate::WineBridge>())
                    .ok_or(VirgoError::WineBridgeNotDownloaded)?
                    .path();
                let workspace = self
                    .layers
                    .prepare_build(&destination, Some(base), job)
                    .await?;
                let executed = execute(
                    InstallInputs {
                        wineprefix: &workspace.wineprefix,
                        runner: runner.as_ref(),
                        winebridge: &winebridge,
                    },
                    &addon.path(),
                    addon.resources(),
                    false,
                    job,
                )
                .await;
                runtime::stop(runner.as_ref(), &workspace.wineprefix).await?;
                self.layers
                    .finish_build(workspace, id, id.to_string(), executed, job)
                    .await
            })
            .await
    }
}

/// Selects the greatest valid semantic version, using UUID as a stable tie-breaker.
fn latest_addon<K>(addons: impl Iterator<Item = Addon<K>>) -> Option<Addon<K>> {
    addons
        .filter_map(|addon| {
            semver::Version::parse(addon.version())
                .ok()
                .map(|version| (version, addon))
        })
        .max_by(|(a, left), (b, right)| a.cmp(b).then_with(|| left.id().cmp(&right.id())))
        .map(|(_, addon)| addon)
}
