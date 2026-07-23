use std::{
    fs, io,
    ops::AsyncFnOnce,
    path::{Path, PathBuf},
};

use fvs_rs::{Layer, Repository, UnmountMode};
use regdiff_rs::prelude::{Diff, Hive, Registry, apply_files};
use uuid::Uuid;

use crate::{
    Context,
    error::{Error, Result},
    runner::{Runner, initialize_and_shutdown_prefix},
};

use super::{
    FVS_BLOCK_SIZE,
    bottle::{BottleType, PrefixStorage},
    error::VirgoError,
};

impl PrefixStorage {
    pub(crate) async fn create(
        kind: BottleType,
        bottle_path: &Path,
        runner: &dyn Runner,
        runner_key: &str,
        context: &Context,
    ) -> Result<Self> {
        match kind {
            BottleType::Standard => {
                initialize_and_shutdown_prefix(runner, &bottle_path.join("prefix")).await?;
                Ok(Self::Standard)
            }
            BottleType::Virgo => {
                fs::create_dir_all(bottle_path.join("upper"))?;
                Ok(Self::Virgo {
                    layers: base_layers(runner, runner_key, context).await?,
                })
            }
        }
    }

    pub(crate) fn kind(&self) -> BottleType {
        match self {
            Self::Standard => BottleType::Standard,
            Self::Virgo { .. } => BottleType::Virgo,
        }
    }

    pub(crate) async fn prepare(&self, bottle_path: &Path, context: &Context) -> Result<()> {
        if let Self::Virgo { layers } = self {
            mount_layers(bottle_path, layers.clone(), context).await?;
        }
        Ok(())
    }

    pub(crate) async fn stop(&self, bottle_path: &Path, context: &Context) -> Result<()> {
        if matches!(self, Self::Virgo { .. }) {
            unmount_prefix(bottle_path, context).await?;
        }
        Ok(())
    }

    pub(crate) async fn rebuild(
        &mut self,
        runner: &dyn Runner,
        runner_key: &str,
        installed: &[Uuid],
        context: &Context,
    ) -> Result<()> {
        let Self::Virgo { layers } = self else {
            return Ok(());
        };
        let mut rebuilt = base_layers(runner, runner_key, context).await?;
        for id in installed {
            rebuilt.push(cached_layer(*id, context).await?);
        }
        *layers = rebuilt;
        Ok(())
    }

    pub(crate) async fn install<F>(
        &mut self,
        bottle_path: &Path,
        item_id: Uuid,
        replaced_id: Option<Uuid>,
        execute: F,
        context: &Context,
    ) -> Result<()>
    where
        F: for<'a> AsyncFnOnce(&'a Path) -> Result<()>,
    {
        match self {
            Self::Standard => execute(&bottle_path.join("prefix")).await,
            Self::Virgo { layers } => {
                install_virgo(bottle_path, layers, item_id, replaced_id, execute, context).await
            }
        }
    }

    pub(crate) async fn uninstall<F>(
        &mut self,
        bottle_path: &Path,
        item_id: Uuid,
        execute: F,
        context: &Context,
    ) -> Result<()>
    where
        F: for<'a> AsyncFnOnce(&'a Path, bool) -> Result<()>,
    {
        match self {
            Self::Standard => execute(&bottle_path.join("prefix"), true).await,
            Self::Virgo { layers } => {
                remove_cached_layer(layers, item_id, context);

                let prefix = bottle_path.join("prefix");
                let cleaned = async {
                    mount_layers(bottle_path, layers.clone(), context).await?;
                    execute(&prefix, false).await
                }
                .await;
                let unmounted = unmount_prefix(bottle_path, context).await;
                cleaned.and(unmounted)
            }
        }
    }
}

fn remove_cached_layer(layers: &mut Vec<Layer>, id: Uuid, context: &Context) {
    let repository = layer_cache(id, context).display().to_string();
    layers.retain(|layer| layer.repository_path != repository);
}

async fn install_virgo<F>(
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
    if !layer_cache(item_id, context).join(".fvs2").is_dir() {
        cache_install(layers.clone(), item_id, execute, context).await?;
    }

    let cached = cached_layer(item_id, context).await?;
    if let Some(id) = replaced_id {
        let replaced = layer_cache(id, context).display().to_string();
        layers.retain(|layer| layer.repository_path != replaced);
    }
    let destination = layer_cache(item_id, context).display().to_string();
    layers.retain(|layer| layer.repository_path != destination);
    layers.push(cached);
    apply_registry(bottle_path, layers, item_id, context).await?;
    Ok(())
}

async fn cache_install<F>(
    layers: Vec<Layer>,
    item_id: Uuid,
    execute: F,
    context: &Context,
) -> Result<()>
where
    F: for<'a> AsyncFnOnce(&'a Path) -> Result<()>,
{
    let destination = layer_cache(item_id, context);
    let registry_destination = registry_cache(item_id, context);
    if destination.exists() {
        fs::remove_dir_all(&destination)?;
    }
    if registry_destination.exists() {
        fs::remove_dir_all(&registry_destination)?;
    }

    // UUID-only caches assume compatible lower layers.
    let stage = context
        .directories()
        .data_dir()
        .join("virgo/.staging")
        .join(Uuid::new_v4().to_string());
    let upper = stage.join("upper");
    let prefix = stage.join("prefix");
    let before = stage.join("before");
    let patches = stage.join("registry");
    for path in [&upper, &prefix, &before, &patches] {
        fs::create_dir_all(path)?;
    }

    let client = context.fvs().await?;
    let result = async {
        let mount = client.mount(&prefix, layers, Some(&upper)).await?;
        let installed = async {
            for (file, _) in registry_files() {
                fs::copy(prefix.join(file), before.join(file))?;
            }
            execute(&prefix).await?;
            for (file, hive) in registry_files() {
                write_forward(
                    &before.join(file),
                    &prefix.join(file),
                    &patches.join(file),
                    hive,
                )?;
            }
            Ok::<_, Error>(())
        }
        .await;
        let pruned: Result<()> = match installed {
            Ok(()) => client
                .diff_mount(&mount, true)
                .await
                .map(drop)
                .map_err(Error::from),
            Err(error) => Err(error),
        };
        let unmounted = client.unmount(&mount, UnmountMode::Normal).await;
        pruned?;
        unmounted?;

        for (file, _) in registry_files() {
            remove_file(&upper.join(file))?;
        }
        let repository = client.new_repository(&upper, FVS_BLOCK_SIZE).await?;
        client.commit(&repository, item_id.to_string()).await?;
        fs::create_dir_all(destination.parent().expect("cache path has a parent"))?;
        fs::create_dir_all(
            registry_destination
                .parent()
                .expect("registry cache path has a parent"),
        )?;
        fs::rename(&patches, &registry_destination)?;
        fs::rename(&upper, &destination)?;
        Ok::<_, Error>(())
    }
    .await;
    let _ = fs::remove_dir_all(stage);
    result
}

async fn apply_registry(
    bottle_path: &Path,
    layers: &[Layer],
    id: Uuid,
    context: &Context,
) -> Result<()> {
    let patches = registry_cache(id, context);
    if !patches.is_dir() {
        return Ok(());
    }

    mount_layers(bottle_path, layers.to_vec(), context).await?;
    let prefix = bottle_path.join("prefix");
    let stage = prefix.join(format!(".bottles-next-registry-{}", Uuid::new_v4()));
    fs::create_dir_all(&stage)?;
    let applied = (|| -> Result<()> {
        for (file, hive) in registry_files() {
            apply_files(
                prefix.join(file),
                patches.join(file),
                stage.join(file),
                hive,
            )
            .map_err(|error| VirgoError::Registry(error.to_string()))?;
        }
        for (file, _) in registry_files() {
            fs::rename(stage.join(file), prefix.join(file))?;
        }
        Ok(())
    })();
    let _ = fs::remove_dir_all(stage);
    let unmounted = unmount_prefix(bottle_path, context).await;
    applied?;
    unmounted
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
    ensure_empty_dir(&prefix)?;
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
    let client = context.fvs().await?;
    let base_path = context.directories().data_dir().join("virgo/base");
    let repository_path = base_path.join("prefix");
    if repository_path.join(".fvs2").is_dir() {
        let repository = client.new_repository(&repository_path, 0).await?;
        let commit = client
            .list_commits(&repository)
            .await?
            .into_iter()
            .next()
            .ok_or(VirgoError::EmptyBase)?;
        return Ok(Layer::from_summary(&repository, Some(&commit)));
    }
    if repository_path.exists() && repository_path.read_dir()?.next().is_some() {
        return Err(VirgoError::DirtyBase(repository_path).into());
    }

    fs::create_dir_all(&repository_path)?;
    if let Err(error) = initialize_and_shutdown_prefix(runner, &repository_path).await {
        let _ = fs::remove_dir_all(&base_path);
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
        let _ = fs::remove_dir_all(base_path);
    }
    committed
}

async fn ensure_adapter(
    runner: &dyn Runner,
    runner_key: &str,
    base: &Layer,
    context: &Context,
) -> Result<Layer> {
    let client = context.fvs().await?;
    let destination = context
        .directories()
        .data_dir()
        .join("virgo/adapters")
        .join(runner_key);
    if destination.join(".fvs2").is_dir() {
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
    fs::create_dir_all(&upper)?;
    fs::create_dir_all(&mountpoint)?;

    let build = async {
        let mount = client
            .mount(&mountpoint, vec![base.clone()], Some(&upper))
            .await?;
        let initialized = initialize_and_shutdown_prefix(runner, &mountpoint).await;
        let unmounted = client.unmount(&mount, UnmountMode::Normal).await;
        initialized?;
        unmounted?;

        let repository = client.new_repository(&upper, FVS_BLOCK_SIZE).await?;
        let commit = client
            .commit(&repository, format!("Runner adapter {runner_key}"))
            .await?;
        fs::create_dir_all(destination.parent().expect("adapter path has a parent"))?;
        fs::rename(&upper, &destination)?;
        Ok::<_, Error>(commit)
    }
    .await;
    let _ = fs::remove_dir_all(stage);

    let commit = build?;
    let repository = Repository {
        repository_path: destination.display().to_string(),
        block_size: FVS_BLOCK_SIZE,
    };
    Ok(Layer::new(&repository, Some(&commit)))
}

async fn cached_layer(id: Uuid, context: &Context) -> Result<Layer> {
    let destination = layer_cache(id, context);
    if !destination.join(".fvs2").is_dir() {
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

fn layer_cache(id: Uuid, context: &Context) -> PathBuf {
    context
        .directories()
        .data_dir()
        .join("virgo/layers")
        .join(id.to_string())
}

fn registry_cache(id: Uuid, context: &Context) -> PathBuf {
    context
        .directories()
        .data_dir()
        .join("virgo/registry")
        .join(id.to_string())
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

fn ensure_empty_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path)?;
    if path.read_dir()?.next().is_some() {
        return Err(VirgoError::DirtyMountpoint(path.to_path_buf()).into());
    }
    Ok(())
}
