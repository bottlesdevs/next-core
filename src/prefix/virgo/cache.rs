//! Shared immutable Virgo layer cache helpers.

use std::{
    fs, io,
    ops::AsyncFnOnce,
    path::{Path, PathBuf},
};

use fvs_rs::Layer;
use regdiff_rs::prelude::{Diff, Hive, Registry, apply_files};
use uuid::Uuid;

use crate::{
    Context,
    bottle::error::VirgoError,
    error::{Error, Result},
    prefix::FVS_BLOCK_SIZE,
};

use super::with_mount;

pub(super) fn remove(layers: &mut Vec<Layer>, id: Uuid, context: &Context) {
    let repository = layer_path(id, context).display().to_string();
    layers.retain(|layer| layer.repository_path != repository);
}

pub(super) async fn exists(id: Uuid, context: &Context) -> Result<bool> {
    let path = layer_path(id, context).join(".fvs2");
    context.spawn_blocking(move || Ok(path.is_dir())).await
}

pub(super) async fn install<F>(
    layers: Vec<Layer>,
    item_id: Uuid,
    execute: F,
    context: &Context,
) -> Result<()>
where
    F: for<'a> AsyncFnOnce(&'a Path) -> Result<()>,
{
    let layer_root = layer_root(context);
    let registry_root = registry_root(context);
    let destination = layer_root.join(item_id.to_string());
    let registry_destination = registry_root.join(item_id.to_string());
    let stage = context
        .directories()
        .data_dir()
        .join("virgo/.staging")
        .join(Uuid::new_v4().to_string());
    let upper = stage.join("upper");
    let prefix = stage.join("prefix");
    let before = stage.join("before");
    let patches = stage.join("registry");

    let setup_destination = destination.clone();
    let setup_registry = registry_destination.clone();
    let setup_paths = [
        upper.clone(),
        prefix.clone(),
        before.clone(),
        patches.clone(),
    ];
    let setup = context
        .spawn_blocking(move || {
            remove_dir_if_exists(&setup_destination)?;
            remove_dir_if_exists(&setup_registry)?;
            for path in setup_paths {
                fs::create_dir_all(path)?;
            }
            Ok(())
        })
        .await;
    if let Err(error) = setup {
        remove_stage(stage, context).await;
        return Err(error);
    }

    let result = async {
        with_mount(&prefix, layers, Some(&upper), context, async |mount| {
            let copy_prefix = prefix.clone();
            let copy_before = before.clone();
            context
                .spawn_blocking(move || {
                    for (file, _) in registry_files() {
                        fs::copy(copy_prefix.join(file), copy_before.join(file))?;
                    }
                    Ok(())
                })
                .await?;

            execute(&prefix).await?;

            let diff_before = before.clone();
            let diff_prefix = prefix.clone();
            let diff_patches = patches.clone();
            context
                .spawn_blocking(move || {
                    for (file, hive) in registry_files() {
                        write_forward(
                            &diff_before.join(file),
                            &diff_prefix.join(file),
                            &diff_patches.join(file),
                            hive,
                        )?;
                    }
                    Ok(())
                })
                .await?;
            context.fvs().await?.diff_mount(mount, true).await?;
            Ok(())
        })
        .await?;

        let clean_upper = upper.clone();
        context
            .spawn_blocking(move || {
                for (file, _) in registry_files() {
                    remove_file(&clean_upper.join(file))?;
                }
                Ok(())
            })
            .await?;
        let client = context.fvs().await?;
        let repository = client.new_repository(&upper, FVS_BLOCK_SIZE).await?;
        client.commit(&repository, item_id.to_string()).await?;

        let move_upper = upper.clone();
        let move_patches = patches.clone();
        context
            .spawn_blocking(move || {
                fs::create_dir_all(layer_root)?;
                fs::create_dir_all(registry_root)?;
                fs::rename(move_patches, registry_destination)?;
                fs::rename(move_upper, destination)?;
                Ok(())
            })
            .await
    }
    .await;
    remove_stage(stage, context).await;
    result
}

pub(super) async fn apply_registry(
    bottle_path: &Path,
    layers: &[Layer],
    id: Uuid,
    context: &Context,
) -> Result<()> {
    let patches = registry_path(id, context);
    let inspect = patches.clone();
    if !context.spawn_blocking(move || Ok(inspect.is_dir())).await? {
        return Ok(());
    }

    let prefix = bottle_path.join("prefix");
    let upper = bottle_path.join("upper");
    with_mount(
        &prefix,
        layers.to_vec(),
        Some(&upper),
        context,
        async |_| {
            let apply_prefix = prefix.clone();
            let stage = prefix.join(format!(".bottles-next-registry-{}", Uuid::new_v4()));
            context
                .spawn_blocking(move || {
                    fs::create_dir_all(&stage)?;
                    let result = (|| {
                        for (file, hive) in registry_files() {
                            apply_files(
                                apply_prefix.join(file),
                                patches.join(file),
                                stage.join(file),
                                hive,
                            )
                            .map_err(|error| VirgoError::Registry(error.to_string()))?;
                        }
                        for (file, _) in registry_files() {
                            fs::rename(stage.join(file), apply_prefix.join(file))?;
                        }
                        Ok::<_, Error>(())
                    })();
                    let _ = fs::remove_dir_all(stage);
                    result
                })
                .await
        },
    )
    .await
}

pub(super) async fn layer(id: Uuid, context: &Context) -> Result<Layer> {
    let destination = layer_path(id, context);
    let inspect = destination.clone();
    if !context
        .spawn_blocking(move || Ok(inspect.join(".fvs2").is_dir()))
        .await?
    {
        return Err(VirgoError::CachedLayerNotFound(destination).into());
    }
    let client = context.fvs().await?;
    let repository = client.new_repository(&destination, 0).await?;
    let commit = client
        .list_commits(&repository)
        .await?
        .into_iter()
        .next()
        .ok_or_else(|| VirgoError::MissingCommit {
            repository: destination,
            state: "HEAD".into(),
        })?;
    Ok(Layer::from_summary(&repository, Some(&commit)))
}

fn layer_root(context: &Context) -> PathBuf {
    context.directories().data_dir().join("virgo/layers")
}

fn registry_root(context: &Context) -> PathBuf {
    context.directories().data_dir().join("virgo/registry")
}

fn layer_path(id: Uuid, context: &Context) -> PathBuf {
    layer_root(context).join(id.to_string())
}

fn registry_path(id: Uuid, context: &Context) -> PathBuf {
    registry_root(context).join(id.to_string())
}

fn registry_files() -> [(&'static str, Hive); 2] {
    [
        ("user.reg", Hive::CurrentUser),
        ("system.reg", Hive::LocalMachine),
    ]
}

fn write_forward(old: &Path, new: &Path, output: &Path, hive: Hive) -> Result<()> {
    let old =
        Registry::try_from(old, hive).map_err(|error| VirgoError::Registry(error.to_string()))?;
    let new =
        Registry::try_from(new, hive).map_err(|error| VirgoError::Registry(error.to_string()))?;
    Registry::diff(&old, &new)
        .serialize_file(output)
        .map_err(|error| VirgoError::Registry(error.to_string()))?;
    Ok(())
}

fn remove_file(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn remove_dir_if_exists(path: &Path) -> io::Result<()> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

async fn remove_stage(stage: PathBuf, context: &Context) {
    let _ = context
        .spawn_blocking(move || {
            remove_dir_if_exists(&stage)?;
            Ok(())
        })
        .await;
}
