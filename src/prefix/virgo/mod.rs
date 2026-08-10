//! Layered Virgo prefix storage.
//!
//! A mounted bottle combines a shared base, a runner-specific adapter, cached
//! addon layers, and the bottle's writable `upper` directory. Layer order is
//! persisted in [`super::Prefix`] and must be changed only while the bottle is
//! stopped.

mod cache;

use std::{
    ops::AsyncFnOnce,
    path::{Path, PathBuf},
};

use futures_lite::StreamExt;
use fvs_rs::{Layer, Mount, Repository, UnmountMode};
use uuid::Uuid;

use crate::{
    Context,
    error::{Error, Result},
    runner::{Runner, initialize_and_shutdown_prefix},
};

use super::FVS_BLOCK_SIZE;
use crate::bottle::error::VirgoError;

pub(super) async fn create(
    bottle_path: &Path,
    runner: &dyn Runner,
    runner_key: &str,
    context: &Context,
) -> Result<Vec<Layer>> {
    let upper = bottle_path.join("upper");
    async_fs::create_dir_all(upper).await?;
    base_layers(runner, runner_key, context).await
}

pub(super) async fn prepare(bottle_path: &Path, layers: &[Layer], context: &Context) -> Result<()> {
    mount_layers(bottle_path, layers.to_vec(), context).await
}

pub(super) async fn stop(bottle_path: &Path, context: &Context) -> Result<()> {
    unmount_prefix(bottle_path, context).await
}

pub(super) async fn rebuild(
    layers: &mut Vec<Layer>,
    runner: &dyn Runner,
    runner_key: &str,
    installed: &[Uuid],
    context: &Context,
) -> Result<()> {
    // Build separately so failure to resolve any cached addon does not partially
    // replace the bottle's persisted layer order.
    let mut rebuilt = base_layers(runner, runner_key, context).await?;
    for id in installed {
        rebuilt.push(cache::layer(*id, context).await?);
    }
    *layers = rebuilt;
    Ok(())
}

pub(super) async fn install<F>(
    bottle_path: &Path,
    layers: &mut Vec<Layer>,
    item_id: Uuid,
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
        cache::install(layers.clone(), item_id, execute, context).await?;
    }

    let cached = cache::layer(item_id, context).await?;
    if let Some(id) = replaced_id {
        cache::remove(layers, id, context);
    }
    cache::remove(layers, item_id, context);
    layers.push(cached);
    cache::apply_registry(bottle_path, layers, item_id, context).await
}

pub(super) async fn uninstall<F>(
    bottle_path: &Path,
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
    let prefix = bottle_path.join("prefix");
    let upper = bottle_path.join("upper");
    with_mount(&prefix, layers.clone(), Some(&upper), context, async |_| {
        execute(&prefix, false).await
    })
    .await
}

/// Mounts for the duration of `work` and always attempts a normal unmount.
///
/// An unmount failure becomes the result only when `work` succeeded. If both
/// fail, the work error is preserved and the unmount failure is logged.
async fn with_mount<F, T>(
    mountpoint: &Path,
    layers: Vec<Layer>,
    upper: Option<&Path>,
    context: &Context,
    work: F,
) -> Result<T>
where
    F: for<'a> AsyncFnOnce(&'a Mount) -> Result<T>,
{
    ensure_empty_dir(mountpoint).await?;
    let client = context.fvs().await?;
    let mount = client.mount(mountpoint, layers, upper).await?;
    let result = work(&mount).await;
    let unmounted = client.unmount(&mount, UnmountMode::Normal).await;

    match result {
        Ok(value) => {
            unmounted?;
            Ok(value)
        }
        Err(error) => {
            if let Err(failed) = unmounted {
                tracing::error!(%failed, "unmount failed after {error}");
            }
            Err(error)
        }
    }
}

/// Prepares a bottle's long-lived Virgo mount.
///
/// An existing mount at the same path is trusted without comparing its layer
/// specification. Callers must stop the bottle before changing persisted layers.
async fn mount_layers(bottle_path: &Path, layers: Vec<Layer>, context: &Context) -> Result<()> {
    let prefix = bottle_path.join("prefix");
    let mountpoint = prefix.display().to_string();
    let client = context.fvs().await?;
    if client.list_mounts().await?.into_iter().any(|mount| {
        mount
            .spec
            .as_ref()
            .is_some_and(|spec| spec.mount_point == mountpoint)
    }) {
        return Ok(());
    }
    ensure_empty_dir(&prefix).await?;
    client
        .mount(&prefix, layers, Some(bottle_path.join("upper")))
        .await?;
    Ok(())
}

async fn unmount_prefix(bottle_path: &Path, context: &Context) -> Result<()> {
    let mountpoint = bottle_path.join("prefix").display().to_string();
    let client = context.fvs().await?;
    if let Some(mount) = client.list_mounts().await?.into_iter().find(|mount| {
        mount
            .spec
            .as_ref()
            .is_some_and(|spec| spec.mount_point == mountpoint)
    }) {
        client.unmount(&mount, UnmountMode::Normal).await?;
    }
    Ok(())
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

/// Loads or creates the single base shared by every Virgo bottle.
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

    if let Err(error) = initialize_and_shutdown_prefix(runner, &repository_path).await {
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

    let build = async {
        with_mount(
            &mountpoint,
            vec![base.clone()],
            Some(&upper),
            context,
            async |_| initialize_and_shutdown_prefix(runner, &mountpoint).await,
        )
        .await?;

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
