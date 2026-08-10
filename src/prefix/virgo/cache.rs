//! Shared immutable Virgo addon cache.
//!
//! Each addon UUID owns an FVS filesystem layer and a separate set of forward
//! registry patches. Registry hives are excluded from the layer so their changes
//! can be merged into each bottle's writable upper directory.

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

/// Removes references from one bottle's stack without deleting the shared cache.
pub(super) fn remove(layers: &mut Vec<Layer>, id: Uuid, context: &Context) {
    let repository = layer_path(id, context).display().to_string();
    layers.retain(|layer| layer.repository_path != repository);
}

/// Checks only for FVS repository metadata; [`layer`] validates its commit.
pub(super) async fn exists(id: Uuid, context: &Context) -> Result<bool> {
    let path = layer_path(id, context).join(".fvs2");
    Ok(async_fs::metadata(path)
        .await
        .is_ok_and(|entry| entry.is_dir()))
}

/// Builds and publishes the cached filesystem layer and registry patches.
///
/// Installation runs in a unique staging mount over the preceding layers. The
/// registry is diffed separately, unchanged filesystem entries are pruned by
/// FVS, and the registry hives are removed before the upper directory is
/// committed as a reusable layer.
///
/// Existing cache entries are removed before the build. Publishing the registry
/// and filesystem destinations requires two renames and is not atomic as a pair;
/// failure may therefore leave only one destination present. Staging cleanup is
/// best-effort.
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
            async_fs::create_dir_all(path).await?;
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
                async_fs::copy(prefix.join(file), before.join(file)).await?;
            }

            execute(&prefix).await?;

            let diff_before = before.clone();
            let diff_prefix = prefix.clone();
            let diff_patches = patches.clone();
            blocking::unblock(move || {
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
            .await?;
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

        async_fs::create_dir_all(layer_root).await?;
        async_fs::create_dir_all(registry_root).await?;
        async_fs::rename(patches, registry_destination).await?;
        async_fs::rename(upper, destination).await?;
        Ok(())
    }
    .await;
    remove_stage(stage).await;
    result
}

/// Merges a cached addon's registry patches into a bottle's writable upper.
///
/// A missing patch directory means the addon has no recorded registry effects.
/// Both replacement hives are prepared in a scratch directory before either is
/// installed, but the final renames are not atomic as a pair. Scratch cleanup is
/// best-effort.
pub(super) async fn apply_registry(
    bottle_path: &Path,
    layers: &[Layer],
    id: Uuid,
    context: &Context,
) -> Result<()> {
    let patches = registry_path(id, context);
    if !async_fs::metadata(&patches)
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
            blocking::unblock(move || {
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

/// Resolves a cached layer and its first available commit.
///
/// Repository metadata without a commit is treated as a corrupt cache entry.
pub(super) async fn layer(id: Uuid, context: &Context) -> Result<Layer> {
    let destination = layer_path(id, context);
    if !async_fs::metadata(destination.join(".fvs2"))
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
    match async_fs::remove_file(path).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

async fn remove_dir_if_exists(path: &Path) -> std::io::Result<()> {
    match async_fs::remove_dir_all(path).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

async fn remove_stage(stage: PathBuf) {
    let _ = remove_dir_if_exists(&stage).await;
}
