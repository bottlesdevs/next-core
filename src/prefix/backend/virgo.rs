//! Selects, builds, and orders immutable Virgo artifacts for a prefix.
//!
//! Frozen addon selections resolve to a base layer, a runner adapter,
//! component layers, and dependency layers in composition order.

use super::Storage;
use crate::{
    Addon, AddonError, Component, Context, Dependency, Job, PrefixState, Runner,
    addons::{InstallInputs, execute},
    error::Result,
    prefix::runtime,
    virgo::{LayerStore, VirgoError, VirgoLayer},
    winebridge::WineBridgeClient,
};
use async_trait::async_trait;
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// Cache key of the base layer shared by every composition.
const BASE: &str = "soda";

/// One layer composed over the base, in composition order.
enum Overlay<'a> {
    Adapter(&'a Addon<Runner>),
    Component(&'a Addon<Component>),
    Dependency(&'a Addon<Dependency>),
}

impl Overlay<'_> {
    /// Returns the layer's cache key and the release it is built from.
    fn key(&self) -> (PathBuf, Uuid) {
        let (collection, id) = match self {
            Self::Adapter(runner) => ("adapters", runner.id()),
            Self::Component(addon) => ("addons", addon.id()),
            Self::Dependency(addon) => ("addons", addon.id()),
        };
        (Path::new(collection).join(id.to_string()), id)
    }
}

/// Lists the overlays of `config`: the runner adapter, components, then
/// dependencies in installation order.
fn overlays(config: &PrefixState) -> impl Iterator<Item = Overlay<'_>> {
    std::iter::once(Overlay::Adapter(&config.runner))
        .chain(config.components.values().map(Overlay::Component))
        .chain(config.dependencies.iter().map(Overlay::Dependency))
}

/// Stores a prefix as a workspace composed from immutable layers.
///
/// Resolves and builds layers with the services of one core instance.
pub(super) struct Virgo {
    cx: Context,
}

#[async_trait]
impl Storage for Virgo {
    /// Builds missing layers and composes the private workspace.
    ///
    /// Failure can leave a partial prefix root and any layers already
    /// published.
    async fn create(&self, state: &PrefixState, root: &Path, job: &Job) -> Result<()> {
        let (base, overlays) = self.prepare_artifacts(state, job).await?;
        self.layers()
            .prepare_workspace(root, &base, &overlays, job)
            .await
    }

    fn changes(&self, before: &PrefixState, after: &PrefixState) -> bool {
        before.components != after.components
            || before.dependencies != after.dependencies
            || before.runner != after.runner
    }

    /// Builds missing layers and recomposes the private workspace.
    async fn apply(
        &self,
        _before: &PrefixState,
        after: &PrefixState,
        root: &Path,
        job: &Job,
    ) -> Result<()> {
        self.create(after, root, job).await
    }

    /// Mounts the cached layers recorded by `state` over the private workspace.
    async fn mount(&self, state: &PrefixState, root: &Path) -> Result<()> {
        let (base, overlays) = self.composition(state).await?;
        self.layers().mount_workspace(root, &base, &overlays).await
    }

    /// Unmounts the workspace, then clears discovery files below and above it.
    async fn release(&self, root: &Path) -> Result<()> {
        self.layers().unmount_workspace(root).await?;
        WineBridgeClient::clear_discovery(&root.join("prefix")).await?;
        WineBridgeClient::clear_discovery(&root.join("upper")).await
    }
}

impl Virgo {
    pub(super) fn new(cx: Context) -> Self {
        Self { cx }
    }

    fn layers(&self) -> &LayerStore {
        &self.cx.fvs().layers
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
    async fn prepare_artifacts(
        &self,
        config: &PrefixState,
        job: &Job,
    ) -> Result<(VirgoLayer, Vec<VirgoLayer>)> {
        let base = self.prepare_base(job).await?;
        let mut built = Vec::new();
        for overlay in overlays(config) {
            let (key, id) = overlay.key();
            built.push(match overlay {
                Overlay::Adapter(_) => self.prepare_adapter(config, &key, id, &base, job).await?,
                Overlay::Component(addon) => self.prepare_addon(addon, &key, &base, job).await?,
                Overlay::Dependency(addon) => self.prepare_addon(addon, &key, &base, job).await?,
            });
        }
        Ok((base, built))
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
    async fn composition(&self, config: &PrefixState) -> Result<(VirgoLayer, Vec<VirgoLayer>)> {
        let base = self.layers().require(Path::new(BASE), None).await?;
        let mut required = Vec::new();
        for overlay in overlays(config) {
            let (key, id) = overlay.key();
            required.push(self.layers().require(&key, Some(id)).await?);
        }
        Ok((base, required))
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
        let destination = Path::new(BASE);
        self.layers()
            .get_or_build(destination, None, job, || async {
                let soda = latest_addon(
                    self.cx
                        .addons()
                        .state()
                        .releases::<Runner>()
                        .filter(|addon| addon.name().eq_ignore_ascii_case("soda")),
                )
                .ok_or(VirgoError::SodaNotDownloaded)?;
                let runner = soda.load_runner(None).await?;
                let workspace = self.layers().prepare_build(destination, None, job).await?;
                let executed = async {
                    job.check_cancellation()?;
                    runner.wineboot(&workspace.wineprefix, "--init").await
                }
                .await;
                runtime::stop(runner.as_ref(), &workspace.wineprefix).await?;
                self.layers()
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
        destination: &Path,
        id: Uuid,
        base: &VirgoLayer,
        job: &Job,
    ) -> Result<VirgoLayer> {
        self.layers()
            .get_or_build(destination, Some(id), job, || async {
                let runner = config.runner.load_runner(config.umu.as_ref()).await?;
                let workspace = self
                    .layers()
                    .prepare_build(destination, Some(base), job)
                    .await?;
                let executed = async {
                    job.check_cancellation()?;
                    runner.wineboot(&workspace.wineprefix, "--init").await
                }
                .await;
                runtime::stop(runner.as_ref(), &workspace.wineprefix).await?;
                self.layers()
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
        destination: &Path,
        base: &VirgoLayer,
        job: &Job,
    ) -> Result<VirgoLayer> {
        let id = addon.id();
        self.layers()
            .get_or_build(destination, Some(id), job, || async {
                let addons = self.cx.addons().state();
                let soda = addons
                    .release::<Runner>(base.id)
                    .ok_or(AddonError::NotFound(base.id))?;
                let runner = soda.load_runner(None).await?;
                let winebridge = latest_addon(addons.releases::<crate::WineBridge>())
                    .ok_or(VirgoError::WineBridgeNotDownloaded)?
                    .path();
                let workspace = self
                    .layers()
                    .prepare_build(destination, Some(base), job)
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
                self.layers()
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
