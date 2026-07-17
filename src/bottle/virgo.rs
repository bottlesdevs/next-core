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
