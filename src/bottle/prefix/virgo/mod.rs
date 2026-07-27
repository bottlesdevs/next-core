mod cache;

use std::{
    fs,
    ops::AsyncFnOnce,
    path::{Path, PathBuf},
};

use fvs_rs::{Layer, Mount, Repository, UnmountMode};
use uuid::Uuid;

use crate::{
    Context,
    error::{Error, Result},
    runner::{Runner, initialize_and_shutdown_prefix},
};

use super::super::FVS_BLOCK_SIZE;
use crate::bottle::error::VirgoError;

pub(super) async fn create(
    bottle_path: &Path,
    runner: &dyn Runner,
    runner_key: &str,
    context: &Context,
) -> Result<Vec<Layer>> {
    let upper = bottle_path.join("upper");
    context
        .spawn_blocking(move || {
            fs::create_dir_all(upper)?;
            Ok(())
        })
        .await?;
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
    cache::remove(layers, item_id, context);
    let prefix = bottle_path.join("prefix");
    let upper = bottle_path.join("upper");
    with_mount(&prefix, layers.clone(), Some(&upper), context, async |_| {
        execute(&prefix, false).await
    })
    .await
}

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
    ensure_empty_dir(mountpoint, context).await?;
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
    ensure_empty_dir(&prefix, context).await?;
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

async fn ensure_base(runner: &dyn Runner, context: &Context) -> Result<Layer> {
    let base_path = context.directories().data_dir().join("virgo/base");
    let repository_path = base_path.join("prefix");
    let inspect = repository_path.clone();
    let cached = context
        .spawn_blocking(move || {
            if inspect.join(".fvs2").is_dir() {
                return Ok(true);
            }
            if inspect.exists() && inspect.read_dir()?.next().is_some() {
                return Err(VirgoError::DirtyBase(inspect).into());
            }
            fs::create_dir_all(inspect)?;
            Ok(false)
        })
        .await?;

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
        remove_dir(base_path, context).await;
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
        remove_dir(base_path, context).await;
    }
    committed
}

async fn ensure_adapter(
    runner: &dyn Runner,
    runner_key: &str,
    base: &Layer,
    context: &Context,
) -> Result<Layer> {
    let root = adapter_root(context);
    let destination = root.join(runner_key);
    let inspect = destination.clone();
    if context
        .spawn_blocking(move || Ok(inspect.join(".fvs2").is_dir()))
        .await?
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
    let create_upper = upper.clone();
    let create_mountpoint = mountpoint.clone();
    context
        .spawn_blocking(move || {
            fs::create_dir_all(create_upper)?;
            fs::create_dir_all(create_mountpoint)?;
            Ok(())
        })
        .await?;

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
        let move_upper = upper.clone();
        let move_destination = destination.clone();
        context
            .spawn_blocking(move || {
                fs::create_dir_all(root)?;
                fs::rename(move_upper, move_destination)?;
                Ok(())
            })
            .await?;
        Ok::<_, Error>(commit)
    }
    .await;
    remove_dir(stage, context).await;

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

async fn ensure_empty_dir(path: &Path, context: &Context) -> Result<()> {
    let path = path.to_path_buf();
    context
        .spawn_blocking(move || {
            fs::create_dir_all(&path)?;
            if path.read_dir()?.next().is_some() {
                return Err(VirgoError::DirtyMountpoint(path).into());
            }
            Ok(())
        })
        .await
}

async fn remove_dir(path: PathBuf, context: &Context) {
    let _ = context
        .spawn_blocking(move || match fs::remove_dir_all(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        })
        .await;
}
