//! Shared immutable bases, runner adapters, and UUID-only addon caches.

mod adapter;
mod build;
mod cache;
mod software;
pub(crate) use adapter::prepare_adapter;
pub(crate) use cache::VirgoLayer;
pub(crate) use software::prepare_addon;

use super::VirgoError;
use crate::environment::prefix::FVS_BLOCK_SIZE;
use crate::{
    Addons, CatalogEntry, Component, Context, Slot,
    error::{Error, Result},
};
use std::path::PathBuf;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

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

/// Resolve the pinned Soda base, creating it under the shared build lock if absent.
pub(crate) async fn prepare_base(
    addons: &Addons,
    cx: &Context,
    cancellation: &CancellationToken,
) -> Result<VirgoLayer> {
    let destination = cx.directories().data_dir().join("virgo/soda");
    if let Some(base) = cache::load(&destination, None).await? {
        return Ok(base);
    }
    let _build = cancellation
        .run_until_cancelled(cx.artifact_build().lock())
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
    let soda = addons
        .component(selected.id())
        .ok_or_else(|| VirgoError::SodaNotDownloaded {
            id: selected.id(),
            version: selected.version().into(),
        })?;
    let runner = soda.addon().load_runner(cx.directories(), None).await?;
    let stage = cx
        .directories()
        .data_dir()
        .join("virgo/.staging")
        .join(Uuid::new_v4().to_string());
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

async fn remove_dir(path: PathBuf) {
    let _ = async_fs::remove_dir_all(path).await;
}
