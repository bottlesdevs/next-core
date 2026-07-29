//! Shared immutable Virgo layer cache helpers.

use std::{
    fs,
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
    Ok(tokio::fs::metadata(path)
        .await
        .is_ok_and(|entry| entry.is_dir()))
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

    let setup = async {
        remove_dir_if_exists(&destination).await?;
        remove_dir_if_exists(&registry_destination).await?;
        for path in [&upper, &prefix, &before, &patches] {
            tokio::fs::create_dir_all(path).await?;
        }
        Ok::<_, Error>(())
    }
    .await;
    if let Err(error) = setup {
        remove_stage(stage).await;
        return Err(error);
    }

    let result = async {
        with_mount(&prefix, layers, Some(&upper), context, async |mount| {
            for (file, _) in registry_files() {
                tokio::fs::copy(prefix.join(file), before.join(file)).await?;
            }

            execute(&prefix).await?;

            let diff_before = before.clone();
            let diff_prefix = prefix.clone();
            let diff_patches = patches.clone();
            tokio::task::spawn_blocking(move || {
                for (file, hive) in registry_files() {
                    write_forward(
                        &diff_before.join(file),
                        &diff_prefix.join(file),
                        &diff_patches.join(file),
                        hive,
                    )?;
                }
                Ok::<_, Error>(())
            })
            .await??;
            context.fvs().await?.diff_mount(mount, true).await?;
            Ok(())
        })
        .await?;

        for (file, _) in registry_files() {
            remove_file(&upper.join(file)).await?;
        }
        let client = context.fvs().await?;
        let repository = client.new_repository(&upper, FVS_BLOCK_SIZE).await?;
        client.commit(&repository, item_id.to_string()).await?;

        tokio::fs::create_dir_all(layer_root).await?;
        tokio::fs::create_dir_all(registry_root).await?;
        tokio::fs::rename(patches, registry_destination).await?;
        tokio::fs::rename(upper, destination).await?;
        Ok(())
    }
    .await;
    remove_stage(stage).await;
    result
}

pub(super) async fn apply_registry(
    bottle_path: &Path,
    layers: &[Layer],
    id: Uuid,
    context: &Context,
) -> Result<()> {
    let patches = registry_path(id, context);
    if !tokio::fs::metadata(&patches)
        .await
        .is_ok_and(|entry| entry.is_dir())
    {
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
            tokio::task::spawn_blocking(move || {
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
            .await?
        },
    )
    .await
}

pub(super) async fn layer(id: Uuid, context: &Context) -> Result<Layer> {
    let destination = layer_path(id, context);
    if !tokio::fs::metadata(destination.join(".fvs2"))
        .await
        .is_ok_and(|entry| entry.is_dir())
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

async fn remove_file(path: &Path) -> std::io::Result<()> {
    match tokio::fs::remove_file(path).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

async fn remove_dir_if_exists(path: &Path) -> std::io::Result<()> {
    match tokio::fs::remove_dir_all(path).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

async fn remove_stage(stage: PathBuf) {
    let _ = remove_dir_if_exists(&stage).await;
}
