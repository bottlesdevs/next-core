//! Shared immutable bases, runner adapters, and UUID-only addon caches.

pub(super) mod cache;
mod software;
pub(crate) use software::prepare_addon;

use super::VirgoError;
use crate::environment::prefix::FVS_BLOCK_SIZE;
use crate::{
    Addon, Addons, CatalogEntry, Component, Context, IndexEntry, Slot,
    error::{Error, Result},
    runner::Runner,
};
use fvs_rs::{Layer, Repository, UnmountMode};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

#[derive(Deserialize, Serialize)]
struct Base {
    soda: Addon<Component>,
    layer: Layer,
}
impl next_config::Config for Base {
    const VERSION: u32 = 1;
}

fn manifest(cx: &Context) -> PathBuf {
    cx.directories().data_dir().join("virgo/base.toml")
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

fn downloaded_soda(
    id: Uuid,
    version: &str,
    addons: &Addons,
) -> Result<std::sync::Arc<IndexEntry<Component>>> {
    addons
        .component(id)
        .filter(|entry| entry.slot() == Slot::Runner && entry.version() == version)
        .ok_or_else(|| {
            VirgoError::SodaNotDownloaded {
                id,
                version: version.into(),
            }
            .into()
        })
}

// Caller holds the shared build mutex, including publication of the manifest.
async fn ensure_base(
    addons: &Addons,
    cx: &Context,
    cancellation: &CancellationToken,
) -> Result<Base> {
    if crate::utils::exists(&manifest(cx)).await? {
        return Ok(next_config::load(manifest(cx)).await?);
    }
    if cancellation.is_cancelled() {
        return Err(Error::Cancelled);
    }
    let entries = addons.component_entries();
    let selected = latest_soda(&entries)?;
    let downloaded = downloaded_soda(selected.id(), selected.version(), addons)?;
    let soda = Addon::from(downloaded.as_ref());
    let runner = soda.load_runner(cx.directories(), None).await?;
    let root = cx.directories().data_dir().join("virgo/soda");
    let prefix = root.join("prefix");
    async_fs::create_dir_all(&root).await?;
    // Never reuse an unpublished prefix: failed shutdown may have left Wine alive.
    async_fs::create_dir(&prefix).await.map_err(|error| {
        std::io::Error::new(
            error.kind(),
            format!("cannot create Soda prefix at {}: {error}", prefix.display()),
        )
    })?;
    let initialized = runner.wineboot(&prefix, "--init").await;
    // Keep storage if Wine cannot be stopped safely.
    crate::environment::shutdown_wine(runner.as_ref(), &prefix).await?;
    let result = async {
        initialized?;
        if cancellation.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let client = cx.fvs().await?;
        let repository = client.new_repository(&prefix, FVS_BLOCK_SIZE).await?;
        let commit = client
            .commit(
                &repository,
                format!("Soda {} ({})", soda.version(), soda.id()),
            )
            .await?;
        let base = Base {
            soda,
            layer: Layer::new(&repository, Some(&commit)),
        };
        if cancellation.is_cancelled() {
            return Err(Error::Cancelled);
        }
        next_config::save(manifest(cx), &base).await?;
        Ok::<_, Error>(base)
    }
    .await;
    if result.is_err() {
        remove_dir(root).await;
    }
    result
}

pub(crate) async fn base_layers(
    runner: &dyn Runner,
    runner_key: &str,
    addons: &Addons,
    cx: &Context,
    cancellation: &CancellationToken,
) -> Result<Vec<Layer>> {
    let _build = cancellation
        .run_until_cancelled(cx.artifact_build().lock())
        .await
        .ok_or(Error::Cancelled)?;
    let base = ensure_base(addons, cx, cancellation).await?;
    let adapter = ensure_adapter(runner, runner_key, &base.layer, cx, cancellation).await?;
    Ok(vec![base.layer, adapter])
}

/// Loads or creates the selected runner adapter over the pinned Soda base.
///
/// Creation is staged over the shared base and published by renaming the
/// committed upper directory into the adapter cache.
async fn ensure_adapter(
    runner: &dyn Runner,
    runner_key: &str,
    base: &Layer,
    context: &Context,
    cancellation: &CancellationToken,
) -> Result<Layer> {
    let root = context.directories().data_dir().join("virgo/soda/adapters");
    let destination = root.join(runner_key);
    if async_fs::metadata(destination.join(".fvs2"))
        .await
        .is_ok_and(|entry| entry.is_dir())
    {
        let client = context.fvs().await?;
        let repository = client.new_repository(&destination, 0).await?;
        let commit = client
            .list_commits(&repository)
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| VirgoError::MissingCommit {
                repository: destination.clone(),
                state: "HEAD".into(),
            })?;
        return Ok(Layer::from_summary(&repository, Some(&commit)));
    }

    if cancellation.is_cancelled() {
        return Err(Error::Cancelled);
    }
    let stage = context
        .directories()
        .data_dir()
        .join("virgo/.staging")
        .join(Uuid::new_v4().to_string());
    let upper = stage.join("upper");
    let mountpoint = stage.join("prefix");
    async_fs::create_dir_all(&upper).await?;
    async_fs::create_dir_all(&mountpoint).await?;

    let client = context.fvs().await?;
    let mount = client
        .mount(&mountpoint, vec![base.clone()], Some(&upper))
        .await?;
    let initialized = runner.wineboot(&mountpoint, "--init").await;
    crate::environment::shutdown_wine(runner, &mountpoint).await?;
    client
        .unmount(&mount, UnmountMode::Normal)
        .await
        .map_err(|source| crate::EnvironmentError::Cleanup {
            prefix: mountpoint.clone(),
            source: Box::new(source.into()),
        })?;
    let build = async {
        initialized?;
        if cancellation.is_cancelled() {
            return Err(Error::Cancelled);
        }

        let client = context.fvs().await?;
        let repository = client.new_repository(&upper, FVS_BLOCK_SIZE).await?;
        let commit = client
            .commit(&repository, format!("Runner adapter {runner_key}"))
            .await?;
        async_fs::create_dir_all(root).await?;
        async_fs::rename(&upper, &destination).await?;
        Ok::<_, Error>(commit)
    }
    .await;
    remove_dir(stage).await;

    let commit = build?;
    let repository = Repository {
        repository_path: destination.display().to_string(),
        block_size: FVS_BLOCK_SIZE,
    };
    Ok(Layer::new(&repository, Some(&commit)))
}

async fn remove_dir(path: PathBuf) {
    let _ = async_fs::remove_dir_all(path).await;
}
