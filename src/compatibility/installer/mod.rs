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

async fn execute_steps(
    bottle: &mut Bottle,
    prefix: &Path,
    resources: &[InstallResource],
) -> Result<()> {
    let runner = bottle.load_runner()?;
    let winebridge = bottle.components.winebridge.path().to_path_buf();
    let mut bridge_client = None;
    let result = async {
        for resource in resources {
            for step in &resource.steps {
                match step {
                    InstallStep::Copy {
                        source,
                        destination,
                    } => {
                        let source = if source.as_os_str().is_empty() {
                            resource.source.clone()
                        } else {
                            resource.source.join(source)
                        };
                        install_file(&source, prefix, destination)?;
                    }
                    InstallStep::Extract { destination } => {
                        extract_into(&resource.source, prefix, destination)?;
                    }
                    InstallStep::Execute { arguments } => {
                        let mut command = RunnerCommand::new(&resource.source);
                        for argument in arguments {
                            command = command.arg(argument);
                        }
                        for (name, value) in &bottle.environment {
                            command = command.env(name, value);
                        }
                        let status = runner.run(prefix, command)?.wait()?;
                        if !status.success() {
                            return Err(io::Error::other(format!(
                                "installer exited with status {status}"
                            ))
                            .into());
                        }
                    }
                    InstallStep::RegisterDlls { dlls } => {
                        for dll in dlls {
                            let mut command = RunnerCommand::new("regsvr32")
                                .arg("/s")
                                .arg(prefix.join(dll).to_string_lossy());
                            for (name, value) in &bottle.environment {
                                command = command.env(name, value);
                            }
                            let status = runner.run(prefix, command)?.wait()?;
                            if !status.success() {
                                return Err(io::Error::other(format!(
                                    "regsvr32 exited with status {status}"
                                ))
                                .into());
                            }
                        }
                    }
                    InstallStep::SetRegistryValue {
                        hive,
                        key,
                        name,
                        value,
                    } => {
                        ensure_bridge(
                            &mut bridge_client,
                            runner.as_ref(),
                            prefix,
                            &winebridge,
                            &bottle.environment,
                        )
                        .await?
                        .set_registry_value(*hive, key.clone(), name.clone(), value.clone())
                        .await?;
                    }
                    InstallStep::SetDllOverrides { dlls, mode } => {
                        let bridge = ensure_bridge(
                            &mut bridge_client,
                            runner.as_ref(),
                            prefix,
                            &winebridge,
                            &bottle.environment,
                        )
                        .await?;
                        for dll in dlls {
                            bridge.set_dll_override(dll.clone(), *mode).await?;
                        }
                    }
                    InstallStep::SetEnvironment { name, value } => {
                        bottle.environment.insert(name.clone(), value.clone());
                        if let Some(bridge) = bridge_client.take() {
                            bridge.shutdown().await?;
                        }
                    }
                }
            }
        }
        Ok::<_, Error>(())
    }
    .await;

    let bridge_stopped = match bridge_client {
        Some(bridge) => bridge.shutdown().await,
        None => Ok(()),
    };
    let runner_stopped = runner.shutdown_prefix(prefix);
    result?;
    bridge_stopped?;
    runner_stopped
}

async fn ensure_bridge<'a>(
    bridge: &'a mut Option<WineBridgeClient>,
    runner: &dyn Runner,
    prefix: &Path,
    executable: &Path,
    environment: &HashMap<String, String>,
) -> Result<&'a WineBridgeClient> {
    if bridge.is_none() {
        *bridge = Some(
            WineBridgeClient::new(
                runner,
                prefix,
                executable.to_path_buf(),
                environment
                    .iter()
                    .map(|(name, value)| (name.into(), value.into())),
            )
            .await?,
        );
    }
    Ok(bridge.as_ref().expect("WineBridge was initialized"))
}

fn install_file(source: &Path, prefix: &Path, relative: &Path) -> Result<()> {
    let destination = prefix.join(relative);
    fs::create_dir_all(destination.parent().expect("destination has a parent"))?;
    let relative_backup = backup_path(relative);
    let backup = prefix.join(&relative_backup);
    if destination.is_file() && !backup.exists() {
        fs::copy(&destination, &backup)?;
    }
    fs::copy(source, destination)?;
    Ok(())
}

fn extract_into(archive: &Path, prefix: &Path, destination: &Path) -> Result<()> {
    let stage = crate::utils::directories::expect()
        .data_dir()
        .join(".staging")
        .join(Uuid::new_v4().to_string());
    fs::create_dir_all(&stage)?;
    let result = (|| -> Result<()> {
        archive::extract(archive, &stage)?;
        for source in archive::files(&stage)? {
            let relative = destination.join(source.strip_prefix(&stage).map_err(io::Error::other)?);
            install_file(&source, prefix, &relative)?;
        }
        Ok(())
    })();
    let _ = fs::remove_dir_all(stage);
    result
}

fn backup_path(path: &Path) -> PathBuf {
    let mut path = path.as_os_str().to_os_string();
    path.push(".bak");
    PathBuf::from(path)
}

fn remove_file(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn registry_files() -> [(&'static str, Hive); 2] {
    [
        ("user.reg", Hive::CurrentUser),
        ("system.reg", Hive::LocalMachine),
    ]
}

fn write_forward(old: &Path, new: &Path, output: &Path, hive: Hive) -> Result<()> {
    let old = Registry::try_from(old, hive).map_err(io::Error::other)?;
    let new = Registry::try_from(new, hive).map_err(io::Error::other)?;
    Registry::diff(&old, &new)
        .serialize_file(output)
        .map_err(io::Error::other)?;
    Ok(())
}

pub(super) fn layer_cache(id: Uuid) -> PathBuf {
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

pub(crate) async fn cached_layer(id: Uuid) -> Result<Layer> {
    let destination = layer_cache(id);
    if !destination.join(".fvs2").is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("cached Virgo layer not found: {}", destination.display()),
        )
        .into());
    }
    let client = fvs().await?;
    let repository = client.new_repository(&destination, 0).await?;
    let commit = client
        .list_commits(&repository)
        .await?
        .into_iter()
        .next()
        .ok_or_else(|| BottleError::MissingCommit {
            repository: destination,
            state: "HEAD".into(),
        })?;
    Ok(Layer::new(&repository, Some(&commit)))
}
