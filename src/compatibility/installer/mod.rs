mod recipes;

use std::{
    collections::HashMap,
    fs, io,
    path::{Path, PathBuf},
};

use fvs_rs::{Layer, UnmountMode};
use regdiff_rs::prelude::{Diff, Hive, Registry, apply_files};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    bottle::{Bottle, BottleError, PrefixStorage, fvs, invalid_components},
    compatibility::components::{
        Component, ComponentManager,
        catalog::{ComponentKind, RunnerKind},
    },
    error::{Error, Result},
    proto::{DllOverrideMode, RegistryHive, registry_value::Value as RegistryValue},
    runner::{Runner, RunnerCommand},
    utils::archive,
    winebridge::WineBridgeClient,
};

use self::super::deserialize_non_empty_string;
pub(super) use recipes::component_steps;

const BLOCK_SIZE: u32 = 1024 * 1024;

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "action", rename_all = "kebab-case", deny_unknown_fields)]
pub enum InstallStep {
    Copy {
        #[serde(default)]
        source: PathBuf,
        destination: PathBuf,
    },
    Execute {
        #[serde(default)]
        arguments: Vec<String>,
    },
    Extract {
        destination: PathBuf,
    },
    RegisterDlls {
        dlls: Vec<PathBuf>,
    },
    SetRegistryValue {
        hive: RegistryHive,
        #[serde(deserialize_with = "deserialize_non_empty_string")]
        key: String,
        name: String,
        value: RegistryValue,
    },
    SetDllOverrides {
        dlls: Vec<String>,
        mode: DllOverrideMode,
    },
    SetEnvironment {
        name: String,
        value: String,
    },
}

#[derive(Clone)]
pub(crate) struct InstallResource {
    pub(crate) source: PathBuf,
    pub(crate) steps: Vec<InstallStep>,
}

pub(crate) trait Installable {
    fn id(&self) -> Uuid;
    fn prepare(&self) -> Result<Vec<InstallResource>>;
}

pub(crate) async fn execute(bottle: &mut Bottle, item: &impl Installable) -> Result<()> {
    let resources = item.prepare()?;
    match &bottle.storage {
        PrefixStorage::Standard => {
            let prefix = bottle.prefix();
            execute_steps(bottle, &prefix, &resources).await?;
        }
        PrefixStorage::Virgo { layers } => {
            if layer_cache(item.id()).join(".fvs2").is_dir() {
                replay_environment(bottle, &resources);
            } else {
                cache_virgo_install(bottle, item, &resources, layers.clone()).await?;
            }
        }
    }
    Ok(())
}

fn replay_environment(bottle: &mut Bottle, resources: &[InstallResource]) {
    for step in resources.iter().flat_map(|resource| &resource.steps) {
        if let InstallStep::SetEnvironment { name, value } = step {
            bottle.environment.insert(name.clone(), value.clone());
        }
    }
}

impl Bottle {
    pub(crate) async fn apply_virgo_registry(&mut self, id: Uuid) -> Result<()> {
        if !matches!(self.storage, PrefixStorage::Virgo { .. }) {
            return Ok(());
        }
        let patches = registry_cache(id);
        if !patches.is_dir() {
            return Ok(());
        }

        self.ensure_mounted().await?;
        let stage = self
            .prefix()
            .join(format!(".bottles-next-registry-{}", Uuid::new_v4()));
        fs::create_dir_all(&stage)?;
        let result = (|| -> Result<()> {
            for (file, hive) in registry_files() {
                apply_files(
                    self.prefix().join(file),
                    patches.join(file),
                    stage.join(file),
                    hive,
                )
                .map_err(|error| io::Error::other(error.to_string()))?;
            }
            for (file, _) in registry_files() {
                fs::rename(stage.join(file), self.prefix().join(file))?;
            }
            Ok(())
        })();
        let _ = fs::remove_dir_all(stage);
        let unmounted = self.unmount().await;
        result?;
        unmounted
    }
}

pub(crate) fn umu_for_runner(
    kind: RunnerKind,
    installed: Option<&Component>,
) -> Result<Option<Component>> {
    if kind == RunnerKind::Wine {
        return Ok(None);
    }
    if let Some(umu) = installed {
        return Ok(Some(umu.clone()));
    }
    ComponentManager::new()?
        .components()
        .iter()
        .filter(|component| component.kind() == ComponentKind::Umu)
        .max_by(|left, right| left.version().cmp(right.version()))
        .cloned()
        .map(Some)
        .ok_or_else(|| invalid_components("Proton runner requires a locally available UMU"))
}

async fn cache_virgo_install(
    bottle: &mut Bottle,
    item: &impl Installable,
    resources: &[InstallResource],
    layers: Vec<Layer>,
) -> Result<()> {
    let destination = layer_cache(item.id());
    let registry_destination = registry_cache(item.id());
    if destination.join(".fvs2").is_dir() {
        return Ok(());
    }
    if destination.exists() {
        fs::remove_dir_all(&destination)?;
    }
    if registry_destination.exists() {
        fs::remove_dir_all(&registry_destination)?;
    }

    // UUID-only caches assume compatible lower layers
    // TODO: hash the lower stack if that assumption ever causes real collisions.
    let stage = crate::utils::directories::expect()
        .data_dir()
        .join("virgo/.staging")
        .join(Uuid::new_v4().to_string());
    let upper = stage.join("upper");
    let prefix = stage.join("prefix");
    let before = stage.join("before");
    let patches = stage.join("registry");
    fs::create_dir_all(&upper)?;
    fs::create_dir_all(&prefix)?;
    fs::create_dir_all(&before)?;
    fs::create_dir_all(&patches)?;

    let client = fvs().await?;
    let result = async {
        let mount = client.mount(&prefix, layers, Some(upper.clone())).await?;
        let installed = async {
            for (file, _) in registry_files() {
                fs::copy(prefix.join(file), before.join(file))?;
            }
            execute_steps(bottle, &prefix, resources).await?;
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
        client.commit(&repository, item.id().to_string()).await?;
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
