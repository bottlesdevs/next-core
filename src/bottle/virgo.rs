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
    error::BottleError,
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
                initialize_and_shutdown_prefix(runner, &bottle_path.join("prefix"))?;
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
    let previous_layers = layers.clone();
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
    if let Err(error) = apply_registry(bottle_path, layers, item_id).await {
        *layers = previous_layers;
        return Err(error);
    }
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
            .map_err(|error| io::Error::other(error.to_string()))?;
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
