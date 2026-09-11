//! Layered Virgo prefix storage.
//!
//! A mounted prefix combines a shared base, a runner-specific adapter, cached
//! addon layers, and the owner's writable `upper` directory. Layer order is
//! persisted by the owner and must be changed only while the owner is
//! stopped.

use crate::environment::artifacts::{self, cache};

use std::{
    ops::AsyncFnOnce,
    path::{Path, PathBuf},
};

use futures_lite::StreamExt;
use fvs_rs::{Layer, UnmountMode};
use uuid::Uuid;

use crate::{Context, error::Result, runner::Runner};

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
    #[error("no Soda runner release in the current component catalog")]
    SodaNotInCatalog,
    #[error("invalid Soda semantic version: {0}")]
    InvalidSodaVersion(String),
    #[error("download Soda {version} ({id}) before building the Virgo base or an addon layer")]
    SodaNotDownloaded { id: Uuid, version: String },
    #[error("cyclic addon prerequisites involving {0}")]
    CyclicPrerequisites(Uuid),
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
    addons: &crate::Addons,
    cancellation: &tokio_util::sync::CancellationToken,
) -> Result<Vec<Layer>> {
    let upper = root.join("upper");
    async_fs::create_dir_all(upper).await?;
    artifacts::base_layers(runner, runner_key, addons, context, cancellation).await
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
    addons: &crate::Addons,
    cancellation: &tokio_util::sync::CancellationToken,
) -> Result<()> {
    // Build separately so failure to resolve any cached addon does not partially
    // replace the owner's persisted layer order.
    let mut rebuilt =
        artifacts::base_layers(runner, runner_key, addons, context, cancellation).await?;
    for id in installed {
        rebuilt.push(cache::layer(*id, context).await?);
    }
    *layers = rebuilt;
    Ok(())
}

pub(super) async fn install(
    root: &Path,
    layers: &mut Vec<Layer>,
    item_id: Uuid,
    replaced_id: Option<Uuid>,
    context: &Context,
) -> Result<()> {
    let cached = cache::layer(item_id, context).await?;
    if let Some(id) = replaced_id {
        cache::remove(layers, id, context);
    }
    cache::remove(layers, item_id, context);
    layers.push(cached);
    prepare(root, layers, context).await?;
    cache::apply_registry(&root.join("prefix"), item_id, context).await
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

/// Refuses to mount over existing contents, which would otherwise be hidden.
async fn ensure_empty_dir(path: &Path) -> Result<()> {
    async_fs::create_dir_all(path).await?;
    if async_fs::read_dir(path).await?.try_next().await?.is_some() {
        return Err(VirgoError::DirtyMountpoint(path.to_path_buf()).into());
    }
    Ok(())
}
