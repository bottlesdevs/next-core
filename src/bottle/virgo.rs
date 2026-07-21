use std::{
    fs, io,
    ops::AsyncFnOnce,
    path::{Path, PathBuf},
};

use fvs_rs::{Layer, Repository, UnmountMode};
use regdiff_rs::prelude::{Diff, Hive, Registry, apply_files};
use uuid::Uuid;

use crate::{
    error::{Error, Result},
    runner::{Runner, initialize_and_shutdown_prefix},
};

use super::{
    bottle::{BottleType, PrefixStorage},
    error::VirgoError,
    manager::fvs,
};

const BLOCK_SIZE: u32 = 1024 * 1024;

impl PrefixStorage {
    pub(crate) async fn create(
        kind: BottleType,
        bottle_path: &Path,
        runner: &dyn Runner,
        runner_key: &str,
    ) -> Result<Self> {
        match kind {
            BottleType::Standard => {
                initialize_and_shutdown_prefix(runner, &bottle_path.join("prefix")).await?;
                Ok(Self::Standard)
            }
            BottleType::Virgo => {
                fs::create_dir_all(bottle_path.join("upper"))?;
                Ok(Self::Virgo {
                    layers: base_layers(runner, runner_key).await?,
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

    pub(crate) async fn prepare(&self, bottle_path: &Path) -> Result<()> {
        if let Self::Virgo { layers } = self {
            mount_layers(bottle_path, layers.clone()).await?;
        }
        Ok(())
    }

    pub(crate) async fn stop(&self, bottle_path: &Path) -> Result<()> {
        if matches!(self, Self::Virgo { .. }) {
            unmount_prefix(bottle_path).await?;
        }
        Ok(())
    }

    pub(crate) async fn rebuild(
        &mut self,
        runner: &dyn Runner,
        runner_key: &str,
        installed: &[Uuid],
    ) -> Result<()> {
        let Self::Virgo { layers } = self else {
            return Ok(());
        };
        let mut rebuilt = base_layers(runner, runner_key).await?;
        for id in installed {
            rebuilt.push(cached_layer(*id).await?);
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
    ) -> Result<bool>
    where
        F: for<'a> AsyncFnOnce(&'a Path) -> Result<()>,
    {
        match self {
            Self::Standard => {
                execute(&bottle_path.join("prefix")).await?;
                Ok(true)
            }
            Self::Virgo { layers } => {
                install_virgo(bottle_path, layers, item_id, replaced_id, execute).await
            }
        }
    }

    pub(crate) async fn uninstall<F>(
        &mut self,
        bottle_path: &Path,
        item_id: Uuid,
        execute: F,
    ) -> Result<()>
    where
        F: for<'a> AsyncFnOnce(&'a Path, bool) -> Result<()>,
    {
        match self {
            Self::Standard => execute(&bottle_path.join("prefix"), true).await,
            Self::Virgo { layers } => {
                remove_cached_layer(layers, item_id);

                let prefix = bottle_path.join("prefix");
                let cleaned = async {
                    mount_layers(bottle_path, layers.clone()).await?;
                    execute(&prefix, false).await
                }
                .await;
                let unmounted = unmount_prefix(bottle_path).await;
                cleaned.and(unmounted)
            }
        }
    }
}

fn remove_cached_layer(layers: &mut Vec<Layer>, id: Uuid) {
    let repository = layer_cache(id).display().to_string();
    layers.retain(|layer| layer.repository_path != repository);
}

async fn install_virgo<F>(
    bottle_path: &Path,
    layers: &mut Vec<Layer>,
    item_id: Uuid,
    replaced_id: Option<Uuid>,
    execute: F,
) -> Result<bool>
where
    F: for<'a> AsyncFnOnce(&'a Path) -> Result<()>,
{
    let executed = if layer_cache(item_id).join(".fvs2").is_dir() {
        false
    } else {
        cache_install(layers.clone(), item_id, execute).await?;
        true
    };

    let cached = cached_layer(item_id).await?;
    if let Some(id) = replaced_id {
        let replaced = layer_cache(id).display().to_string();
        layers.retain(|layer| layer.repository_path != replaced);
    }
    let destination = layer_cache(item_id).display().to_string();
    layers.retain(|layer| layer.repository_path != destination);
    layers.push(cached);
    apply_registry(bottle_path, layers, item_id).await?;
    Ok(executed)
}

async fn cache_install<F>(layers: Vec<Layer>, item_id: Uuid, execute: F) -> Result<()>
where
    F: for<'a> AsyncFnOnce(&'a Path) -> Result<()>,
{
    let destination = layer_cache(item_id);
    let registry_destination = registry_cache(item_id);
    if destination.exists() {
        fs::remove_dir_all(&destination)?;
    }
    if registry_destination.exists() {
        fs::remove_dir_all(&registry_destination)?;
    }

    // UUID-only caches assume compatible lower layers.
    let stage = crate::utils::directories::expect()
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

    let client = fvs().await?;
    let lower = layers.clone();
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
        let unmounted = client.unmount(&mount, UnmountMode::Normal).await;
        installed?;
        unmounted?;

        for (file, _) in registry_files() {
            remove_file(&upper.join(file))?;
        }
        prune_identical_to_lower(client, &stage, &upper, lower).await?;
        let repository = client.new_repository(&upper, BLOCK_SIZE).await?;
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

const WHITEOUT_PREFIX: &str = ".wh.";

async fn prune_identical_to_lower(
    client: &fvs_rs::Fvs2dClient,
    stage: &Path,
    upper: &Path,
    lower: Vec<Layer>,
) -> Result<()> {
    let view = stage.join("lower");
    fs::create_dir_all(&view)?;
    let result = async {
        let mount = client.mount(&view, lower, None::<&Path>).await?;
        let pruned = prune_dir(upper, upper, &view);
        let unmounted = client.unmount(&mount, UnmountMode::Normal).await;
        pruned?;
        unmounted?;
        Ok::<_, Error>(())
    }
    .await;
    let _ = fs::remove_dir_all(&view);
    result
}

fn prune_dir(root: &Path, dir: &Path, lower: &Path) -> Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            prune_dir(root, &path, lower)?;
            if fs::read_dir(&path)?.next().is_none() {
                let _ = fs::remove_dir(&path);
            }
            continue;
        }
        if entry.file_name().to_string_lossy().starts_with(WHITEOUT_PREFIX) {
            continue;
        }
        let relative = path.strip_prefix(root).expect("upper entry under root");
        if same_entry(&path, &lower.join(relative))? {
            fs::remove_file(&path)?;
        }
    }
    Ok(())
}

fn same_entry(upper: &Path, lower: &Path) -> Result<bool> {
    let upper_meta = fs::symlink_metadata(upper)?;
    let lower_meta = match fs::symlink_metadata(lower) {
        Ok(meta) => meta,
        Err(_) => return Ok(false),
    };
    if upper_meta.file_type().is_symlink() || lower_meta.file_type().is_symlink() {
        return Ok(upper_meta.file_type().is_symlink()
            && lower_meta.file_type().is_symlink()
            && fs::read_link(upper)? == fs::read_link(lower)?);
    }
    if !upper_meta.file_type().is_file() || !lower_meta.file_type().is_file() {
        return Ok(false);
    }
    Ok(upper_meta.len() == lower_meta.len() && fs::read(upper)? == fs::read(lower)?)
}

async fn apply_registry(bottle_path: &Path, layers: &[Layer], id: Uuid) -> Result<()> {
    let patches = registry_cache(id);
    if !patches.is_dir() {
        return Ok(());
    }

    mount_layers(bottle_path, layers.to_vec()).await?;
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
    let unmounted = unmount_prefix(bottle_path).await;
    applied?;
    unmounted
}

async fn mount_layers(bottle_path: &Path, layers: Vec<Layer>) -> Result<()> {
    let prefix = bottle_path.join("prefix");
    let mountpoint = prefix.display().to_string();
    let client = fvs().await?;
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

async fn unmount_prefix(bottle_path: &Path) -> Result<()> {
    let mountpoint = bottle_path.join("prefix").display().to_string();
    let client = fvs().await?;
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

async fn base_layers(runner: &dyn Runner, runner_key: &str) -> Result<Vec<Layer>> {
    let base = ensure_base(runner).await?;
    let adapter = ensure_adapter(runner, runner_key, &base).await?;
    Ok(vec![base, adapter])
}

async fn ensure_base(runner: &dyn Runner) -> Result<Layer> {
    let client = fvs().await?;
    let base_path = crate::utils::directories::expect()
        .data_dir()
        .join("virgo/base");
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
        let repository = client.new_repository(&repository_path, BLOCK_SIZE).await?;
        let commit = client.commit(&repository, "Virgo base".into()).await?;
        Ok(Layer::new(&repository, Some(&commit)))
    }
    .await;
    if committed.is_err() {
        let _ = fs::remove_dir_all(base_path);
    }
    committed
}

async fn ensure_adapter(runner: &dyn Runner, runner_key: &str, base: &Layer) -> Result<Layer> {
    let client = fvs().await?;
    let destination = crate::utils::directories::expect()
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

    let stage = crate::utils::directories::expect()
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

        let repository = client.new_repository(&upper, BLOCK_SIZE).await?;
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
        block_size: BLOCK_SIZE,
    };
    Ok(Layer::new(&repository, Some(&commit)))
}

async fn cached_layer(id: Uuid) -> Result<Layer> {
    let destination = layer_cache(id);
    if !destination.join(".fvs2").is_dir() {
        return Err(VirgoError::CachedLayerNotFound(destination).into());
    }
    let client = fvs().await?;
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

fn layer_cache(id: Uuid) -> PathBuf {
    crate::utils::directories::expect()
        .data_dir()
        .join("virgo/layers")
        .join(id.to_string())
}

fn registry_cache(id: Uuid) -> PathBuf {
    crate::utils::directories::expect()
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
