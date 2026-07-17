mod recipes;

use std::{
    collections::HashMap,
    fs, io,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    bottle::Bottle,
    compatibility::components::{
        Component, ComponentManager,
        catalog::{ComponentKind, RunnerKind},
    },
    error::{Error, Result, ResultExt},
    proto::{DllOverrideMode, RegistryHive, registry_value::Value as RegistryValue},
    runner::{Runner, RunnerCommand},
    utils::archive,
    winebridge::WineBridgeClient,
};

use self::super::deserialize_non_empty_string;
pub(super) use recipes::component_steps;

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

pub(crate) async fn execute(
    bottle: &mut Bottle,
    item: &impl Installable,
    replaced_id: Option<Uuid>,
) -> Result<()> {
    let resources = item.prepare()?;
    let previous_environment = bottle.environment.clone();
    let runner = bottle.load_runner()?;
    let winebridge = bottle.components.winebridge.path().to_path_buf();
    let bottle_path = bottle.bottle_path();
    let storage = &mut bottle.storage;
    let environment = &mut bottle.environment;
    let installed = storage
        .install(&bottle_path, item.id(), replaced_id, async |prefix| {
            execute_steps(
                runner.as_ref(),
                prefix,
                &winebridge,
                environment,
                &resources,
            )
            .await
        })
        .await;
    match installed {
        Ok(true) => Ok(()),
        Ok(false) => {
            replay_environment(&mut bottle.environment, &resources);
            Ok(())
        }
        Err(error) => {
            bottle.environment = previous_environment;
            Err(error)
        }
    }
}

pub(crate) async fn uninstall(bottle: &mut Bottle, component: &Component) -> Result<()> {
    let resources = component.prepare()?;
    let runner = bottle.load_runner()?;
    let winebridge = bottle.components.winebridge.path().to_path_buf();
    let bottle_path = bottle.bottle_path();
    let previous_environment = bottle.environment.clone();
    let result = {
        let storage = &mut bottle.storage;
        let environment = &mut bottle.environment;
        storage
            .uninstall(
                &bottle_path,
                component.id(),
                async |prefix, restore_files| {
                    uninstall_steps(
                        runner.as_ref(),
                        prefix,
                        &winebridge,
                        environment,
                        &resources,
                        restore_files,
                        component.id(),
                    )
                    .await
                },
            )
            .await
    };
    if result.is_err() {
        bottle.environment = previous_environment;
    }
    result
}

fn replay_environment(environment: &mut HashMap<String, String>, resources: &[InstallResource]) {
    for step in resources.iter().flat_map(|resource| &resource.steps) {
        if let InstallStep::SetEnvironment { name, value } = step {
            environment.insert(name.clone(), value.clone());
        }
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
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                String::from("Proton runner requires a locally available UMU"),
            )
            .into()
        })
}

async fn execute_steps(
    runner: &dyn Runner,
    prefix: &Path,
    winebridge: &Path,
    environment: &mut HashMap<String, String>,
    resources: &[InstallResource],
) -> Result<()> {
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
                        for (name, value) in environment.iter() {
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
                            for (name, value) in environment.iter() {
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
                        ensure_bridge(&mut bridge_client, runner, prefix, winebridge, environment)
                            .await?
                            .set_registry_value(*hive, key.clone(), name.clone(), value.clone())
                            .await?;
                    }
                    InstallStep::SetDllOverrides { dlls, mode } => {
                        let bridge = ensure_bridge(
                            &mut bridge_client,
                            runner,
                            prefix,
                            winebridge,
                            environment,
                        )
                        .await?;
                        for dll in dlls {
                            bridge.set_dll_override(dll.clone(), *mode).await?;
                        }
                    }
                    InstallStep::SetEnvironment { name, value } => {
                        environment.insert(name.clone(), value.clone());
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

async fn uninstall_steps(
    runner: &dyn Runner,
    prefix: &Path,
    winebridge: &Path,
    environment: &mut HashMap<String, String>,
    resources: &[InstallResource],
    restore_files: bool,
    component_id: Uuid,
) -> Result<()> {
    let mut bridge_client = None;

    for resource in resources.iter().rev() {
        for step in resource.steps.iter().rev() {
            match step {
                InstallStep::Copy { destination, .. } if restore_files => {
                    uninstall_file(prefix, destination).log_warn();
                }
                InstallStep::Copy { .. } => {}
                InstallStep::SetEnvironment { name, .. } => {
                    environment.remove(name);
                }
                InstallStep::SetDllOverrides { dlls, .. } => {
                    let bridge = match ensure_bridge(
                        &mut bridge_client,
                        runner,
                        prefix,
                        winebridge,
                        environment,
                    )
                    .await
                    {
                        Ok(bridge) => bridge,
                        Err(error) => {
                            tracing::warn!(%error);
                            continue;
                        }
                    };
                    for dll in dlls.iter().rev() {
                        match bridge.delete_dll_override(dll.clone()).await {
                            Err(error) if is_not_found(&error) => {}
                            result => {
                                result.log_warn();
                            }
                        }
                    }
                }
                unsupported => {
                    tracing::warn!(
                        %component_id,
                        step = ?unsupported,
                        "skipping unsupported component uninstall action"
                    );
                }
            }
        }
    }

    if let Some(bridge) = bridge_client {
        bridge.shutdown().await.log_warn();
    }
    runner.shutdown_prefix(prefix).log_warn();
    Ok(())
}

fn is_not_found(error: &Error) -> bool {
    matches!(error, Error::Status(status) if status.code() == tonic::Code::NotFound)
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

fn uninstall_file(prefix: &Path, relative: &Path) -> io::Result<()> {
    let destination = prefix.join(relative);
    let backup = prefix.join(backup_path(relative));
    if backup.is_file() {
        fs::copy(&backup, &destination)?;
        fs::remove_file(backup)
    } else {
        match fs::remove_file(destination) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }
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
