//! Layered Virgo prefix storage.
//!
//! A mounted prefix combines a shared base, a runner-specific adapter, cached
//! addon layers, and the owner's writable `upper` directory. Layer order is
//! persisted by the owner and must be changed only while the owner is
//! stopped.

mod cache;

use std::{
    ops::AsyncFnOnce,
    path::{Path, PathBuf},
};

use futures_lite::StreamExt;
use fvs_rs::{Layer, Repository, UnmountMode};
use uuid::Uuid;

use crate::{
    Context,
    error::{Error, Result},
    runner::Runner,
};

use super::FVS_BLOCK_SIZE;

/// Virgo-specific failures carried by [`crate::error::Error::Virgo`].
#[derive(Debug, thiserror::Error)]
pub enum VirgoError {
    /// A required FVS commit is missing from a repository.
    #[error("FVS repository {repository} has no commit {state}")]
    MissingCommit {
        /// Repository whose history was searched.
        repository: PathBuf,
        /// Requested full or abbreviated state ID.
        state: String,
    },
    /// An existing Virgo base repository has no commits to use as a layer.
    #[error("Virgo base exists but has no commits")]
    EmptyBase,
    /// Virgo cannot initialize a base over an existing nonempty directory.
    #[error("refusing to initialize non-empty Virgo base at {0}")]
    DirtyBase(PathBuf),
    /// Virgo cannot mount a prefix over a nonempty mountpoint.
    #[error("mountpoint is not empty: {0}")]
    DirtyMountpoint(PathBuf),
    #[error(
        "mounted layers or writable upper differ from saved configuration at {0}; call stop() and retry"
    )]
    MountMismatch(PathBuf),
    /// A cached layer required to construct the prefix is missing.
    #[error("cached Virgo layer was not found: {0}")]
    CachedLayerNotFound(PathBuf),
    /// Registry data could not be converted while building a Virgo layer.
    #[error("failed to process Virgo registry data: {0}")]
    Registry(String),
}

pub(super) async fn create(
    root: &Path,
    runner: &dyn Runner,
    runner_key: &str,
    context: &Context,
) -> Result<Vec<Layer>> {
    let upper = root.join("upper");
    async_fs::create_dir_all(upper).await?;
    base_layers(runner, runner_key, context).await
}

pub(super) async fn prepare(root: &Path, layers: &[Layer], context: &Context) -> Result<()> {
    if !existing_mount(root, layers, context).await? {
        let prefix = root.join("prefix");
        ensure_empty_dir(&prefix).await?;
        context
            .fvs()
            .await?
            .mount(&prefix, layers.to_vec(), Some(root.join("upper")))
            .await?;
    }
    Ok(())
}

async fn existing_mount(root: &Path, layers: &[Layer], context: &Context) -> Result<bool> {
    let prefix = root.join("prefix");
    let mounts = context.fvs().await?.list_mounts().await?;
    let Some(spec) = mounts
        .into_iter()
        .filter_map(|mount| mount.spec)
        .find(|spec| spec.mount_point == prefix.to_string_lossy())
    else {
        return Ok(false);
    };
    if spec.layers != layers
        || spec.upper_path.as_deref() != Some(root.join("upper").to_string_lossy().as_ref())
    {
        return Err(VirgoError::MountMismatch(prefix).into());
    }
    Ok(true)
}

pub(super) async fn stop(root: &Path, context: &Context) -> Result<()> {
    let prefix = root.join("prefix");
    let client = context.fvs().await?;
    if let Some(mount) = client.list_mounts().await?.into_iter().find(|mount| {
        mount
            .spec
            .as_ref()
            .is_some_and(|spec| spec.mount_point == prefix.to_string_lossy())
    }) {
        client
            .unmount(&mount, UnmountMode::Normal)
            .await
            .map_err(|source| crate::EnvironmentError::Cleanup {
                prefix,
                source: Box::new(source.into()),
            })?;
    }
    Ok(())
}

pub(super) async fn rebuild(
    layers: &mut Vec<Layer>,
    runner: &dyn Runner,
    runner_key: &str,
    installed: &[Uuid],
    context: &Context,
) -> Result<()> {
    // Build separately so failure to resolve any cached addon does not partially
    // replace the owner's persisted layer order.
    let mut rebuilt = base_layers(runner, runner_key, context).await?;
    for id in installed {
        rebuilt.push(cache::layer(*id, context).await?);
    }
    *layers = rebuilt;
    Ok(())
}

pub(super) async fn install<F>(
    root: &Path,
    layers: &mut Vec<Layer>,
    item_id: Uuid,
    runner: &dyn Runner,
    replaced_id: Option<Uuid>,
    execute: F,
    context: &Context,
) -> Result<()>
where
    F: for<'a> AsyncFnOnce(&'a Path) -> Result<()>,
{
    // A cache hit deliberately skips the recipe. The cached filesystem layer and
    // registry patch must therefore capture every prefix effect of installation.
    if !cache::exists(item_id, context).await? {
        cache::install(layers.clone(), item_id, runner, execute, context).await?;
    }

    let cached = cache::layer(item_id, context).await?;
    if let Some(id) = replaced_id {
        cache::remove(layers, id, context);
    }
    cache::remove(layers, item_id, context);
    layers.push(cached);
    cache::apply_registry(root, layers, item_id, context).await
}

pub(super) async fn uninstall<F>(
    root: &Path,
    layers: &mut Vec<Layer>,
    item_id: Uuid,
    execute: F,
    context: &Context,
) -> Result<()>
where
    F: for<'a> AsyncFnOnce(&'a Path, bool) -> Result<()>,
{
    // Removing the layer reveals the previous filesystem contents, so the recipe
    // must not restore overwritten files into the writable upper directory.
    cache::remove(layers, item_id, context);
    prepare(root, layers, context).await?;
    // The enclosing transaction shuts Wine down and unmounts before rollback.
    execute(&root.join("prefix"), false).await
}

async fn base_layers(
    runner: &dyn Runner,
    runner_key: &str,
    context: &Context,
) -> Result<Vec<Layer>> {
    let base = ensure_base(runner, context).await?;
    let adapter = ensure_adapter(runner, runner_key, &base, context).await?;
    Ok(vec![base, adapter])
}

/// Loads or creates the single base shared by every Virgo owner.
///
/// Once the base repository exists, `runner` is not used. A nonempty directory
/// without an FVS repository is rejected rather than overwritten.
async fn ensure_base(runner: &dyn Runner, context: &Context) -> Result<Layer> {
    let base_path = context.directories().data_dir().join("virgo/base");
    let repository_path = base_path.join("prefix");
    let cached = if async_fs::metadata(repository_path.join(".fvs2"))
        .await
        .is_ok_and(|entry| entry.is_dir())
    {
        true
    } else {
        if crate::utils::exists(&repository_path).await?
            && async_fs::read_dir(&repository_path)
                .await?
                .try_next()
                .await?
                .is_some()
        {
            return Err(VirgoError::DirtyBase(repository_path).into());
        }
        async_fs::create_dir_all(&repository_path).await?;
        false
    };

    let client = context.fvs().await?;
    if cached {
        let repository = client.new_repository(&repository_path, 0).await?;
        let commit = client
            .list_commits(&repository)
            .await?
            .into_iter()
            .next()
            .ok_or(VirgoError::EmptyBase)?;
        return Ok(Layer::from_summary(&repository, Some(&commit)));
    }

    let initialized = runner.wineboot(&repository_path, "--init").await;
    crate::environment::shutdown_wine(runner, &repository_path).await?;
    if let Err(error) = initialized {
        remove_dir(base_path).await;
        return Err(error);
    }
    let committed = async {
        let repository = client
            .new_repository(&repository_path, FVS_BLOCK_SIZE)
            .await?;
        let commit = client.commit(&repository, "Virgo base".into()).await?;
        Ok(Layer::new(&repository, Some(&commit)))
    }
    .await;
    if committed.is_err() {
        remove_dir(base_path).await;
    }
    committed
}

/// Loads or creates the adapter cache identified solely by `runner_key`.
///
/// Creation is staged over the shared base and published by renaming the
/// committed upper directory into the adapter cache.
async fn ensure_adapter(
    runner: &dyn Runner,
    runner_key: &str,
    base: &Layer,
    context: &Context,
) -> Result<Layer> {
    let root = adapter_root(context);
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

fn adapter_root(context: &Context) -> PathBuf {
    context.directories().data_dir().join("virgo/adapters")
}

/// Refuses to mount over existing contents, which would otherwise be hidden.
async fn ensure_empty_dir(path: &Path) -> Result<()> {
    async_fs::create_dir_all(path).await?;
    if async_fs::read_dir(path).await?.try_next().await?.is_some() {
        return Err(VirgoError::DirtyMountpoint(path.to_path_buf()).into());
    }
    Ok(())
}

async fn remove_dir(path: PathBuf) {
    let _ = async_fs::remove_dir_all(path).await;
}
