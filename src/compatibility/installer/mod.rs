mod recipes;

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{
    Context,
    error::{Error, Result, ResultExt},
    prefix::{PrefixProgress, TransactionProgress},
    proto::{DllOverrideMode, RegistryHive, registry_value::Value as RegistryValue},
    runner::{Command, Runner, Spawnable, shutdown_prefix},
    utils::{archive, environment::Environment},
    winebridge::WineBridgeClient,
};

use self::super::deserialize_non_empty_string;
pub(super) use recipes::component_steps;

#[derive(Debug, Error)]
pub enum InstallerError {
    #[error("installer exited with status {0}")]
    InstallerFailed(std::process::ExitStatus),
    #[error("regsvr32 exited with status {0}")]
    RegisterDllFailed(std::process::ExitStatus),
    #[error("staged file {path} is outside staging directory {stage}")]
    FileOutsideStage { path: PathBuf, stage: PathBuf },
}

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

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InstallProgress {
    AutoCheckpoint(TransactionProgress),
    Restore(TransactionProgress),
    Copy,
    Execute,
    Extract,
    RegisterDlls,
    SetRegistryValue,
    SetDllOverrides,
    SetEnvironment,
}

impl From<PrefixProgress> for InstallProgress {
    fn from(progress: PrefixProgress) -> Self {
        match progress {
            PrefixProgress::AutoCheckpoint(progress) => Self::AutoCheckpoint(progress),
            PrefixProgress::Restore(progress) => Self::Restore(progress),
        }
    }
}

impl From<&InstallStep> for InstallProgress {
    fn from(step: &InstallStep) -> Self {
        match step {
            InstallStep::Copy { .. } => Self::Copy,
            InstallStep::Execute { .. } => Self::Execute,
            InstallStep::Extract { .. } => Self::Extract,
            InstallStep::RegisterDlls { .. } => Self::RegisterDlls,
            InstallStep::SetRegistryValue { .. } => Self::SetRegistryValue,
            InstallStep::SetDllOverrides { .. } => Self::SetDllOverrides,
            InstallStep::SetEnvironment { .. } => Self::SetEnvironment,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UninstallProgress {
    AutoCheckpoint(TransactionProgress),
    Restore(TransactionProgress),
    RevertFile,
    RemoveDllOverrides,
    RemoveEnvironmentVariable,
    SkipUnsupported,
}

impl From<PrefixProgress> for UninstallProgress {
    fn from(progress: PrefixProgress) -> Self {
        match progress {
            PrefixProgress::AutoCheckpoint(progress) => Self::AutoCheckpoint(progress),
            PrefixProgress::Restore(progress) => Self::Restore(progress),
        }
    }
}

impl From<&InstallStep> for UninstallProgress {
    fn from(step: &InstallStep) -> Self {
        match step {
            InstallStep::Copy { .. } => Self::RevertFile,
            InstallStep::SetDllOverrides { .. } => Self::RemoveDllOverrides,
            InstallStep::SetEnvironment { .. } => Self::RemoveEnvironmentVariable,
            _ => Self::SkipUnsupported,
        }
    }
}

#[derive(Clone)]
pub(crate) struct InstallResource {
    pub(crate) source: PathBuf,
    pub(crate) steps: Vec<InstallStep>,
}

pub(crate) trait Installable {
    fn prepare(&self, directories: &crate::Directories) -> Result<Vec<InstallResource>>;
}

pub(crate) struct InstallInputs<'a> {
    pub(crate) prefix: &'a Path,
    pub(crate) runner: &'a dyn Runner,
    pub(crate) winebridge: &'a Path,
    pub(crate) environment: &'a mut Environment,
}

pub(crate) async fn execute(
    context: &Context,
    inputs: InstallInputs<'_>,
    resources: &[InstallResource],
    cancellation: &CancellationToken,
    on_step: impl Fn(&InstallStep) + Send,
) -> Result<()> {
    let InstallInputs {
        prefix,
        runner,
        winebridge,
        environment,
    } = inputs;
    let result = async {
        check_cancellation(cancellation)?;
        for resource in resources {
            for step in &resource.steps {
                on_step(step);
                execute_step(
                    context,
                    InstallInputs {
                        prefix,
                        runner,
                        winebridge,
                        environment: &mut *environment,
                    },
                    resource,
                    step,
                    cancellation,
                )
                .await?;
                check_cancellation(cancellation)?;
            }
        }
        Ok::<_, Error>(())
    }
    .await;

    let bridge_stopped = shutdown_bridge(prefix).await;
    let runner_stopped = shutdown_prefix(runner, prefix).await;
    result?;
    bridge_stopped?;
    runner_stopped
}

pub(crate) async fn uninstall(
    inputs: InstallInputs<'_>,
    resources: &[InstallResource],
    restore_files: bool,
    item_id: Uuid,
    cancellation: &CancellationToken,
    on_step: impl Fn(&InstallStep) + Send,
) -> Result<()> {
    let InstallInputs {
        prefix,
        runner,
        winebridge,
        environment,
    } = inputs;

    let result = async {
        check_cancellation(cancellation)?;
        for resource in resources.iter().rev() {
            for step in resource.steps.iter().rev() {
                on_step(step);
                uninstall_step(
                    InstallInputs {
                        prefix,
                        runner,
                        winebridge,
                        environment: &mut *environment,
                    },
                    step,
                    restore_files,
                    item_id,
                    cancellation,
                )
                .await?;
                check_cancellation(cancellation)?;
            }
        }
        Ok(())
    }
    .await;

    shutdown_bridge(prefix).await.log_warn();
    shutdown_prefix(runner, prefix).await.log_warn();
    result
}

pub(crate) fn replay_environment(environment: &mut Environment, resources: &[InstallResource]) {
    for step in resources.iter().flat_map(|resource| &resource.steps) {
        if let InstallStep::SetEnvironment { name, value } = step {
            environment.insert(name.clone(), value.clone());
        }
    }
}

async fn execute_step(
    context: &Context,
    inputs: InstallInputs<'_>,
    resource: &InstallResource,
    step: &InstallStep,
    cancellation: &CancellationToken,
) -> Result<()> {
    let InstallInputs {
        prefix,
        runner,
        winebridge,
        environment,
    } = inputs;
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
            let archive = resource.source.clone();
            let prefix = prefix.to_path_buf();
            let destination = destination.clone();
            context
                .spawn_blocking(move || extract_into(&archive, &prefix, &destination))
                .await?;
        }
        InstallStep::Execute { arguments } => {
            let mut command = Command::new(&resource.source);
            for argument in arguments {
                command = command.arg(argument);
            }
            for (name, value) in environment.iter() {
                command = command.env(name, value);
            }
            let status =
                wait_for_child(runner.command(prefix, command).spawn()?, cancellation).await?;
            if !status.success() {
                return Err(InstallerError::InstallerFailed(status).into());
            }
        }
        InstallStep::RegisterDlls { dlls } => {
            for dll in dlls {
                check_cancellation(cancellation)?;
                let mut command = Command::new("regsvr32").arg("/s").arg(prefix.join(dll));
                for (name, value) in environment.iter() {
                    command = command.env(name, value);
                }
                let status =
                    wait_for_child(runner.command(prefix, command).spawn()?, cancellation).await?;
                if !status.success() {
                    return Err(InstallerError::RegisterDllFailed(status).into());
                }
            }
        }
        InstallStep::SetRegistryValue {
            hive,
            key,
            name,
            value,
        } => {
            let command =
                WineBridgeClient::command(runner, prefix, winebridge).envs(environment.iter());
            let bridge = WineBridgeClient::connect_or_spawn(prefix, command).await?;
            check_cancellation(cancellation)?;
            bridge
                .set_registry_value(*hive, key.clone(), name.clone(), value.clone())
                .await?;
        }
        InstallStep::SetDllOverrides { dlls, mode } => {
            let command =
                WineBridgeClient::command(runner, prefix, winebridge).envs(environment.iter());
            let bridge = WineBridgeClient::connect_or_spawn(prefix, command).await?;
            for dll in dlls {
                check_cancellation(cancellation)?;
                bridge.set_dll_override(dll.clone(), *mode).await?;
            }
        }
        InstallStep::SetEnvironment { name, value } => {
            environment.insert(name.clone(), value.clone());
            shutdown_bridge(prefix).await?;
        }
    }
    Ok(())
}

async fn uninstall_step(
    inputs: InstallInputs<'_>,
    step: &InstallStep,
    restore_files: bool,
    component_id: Uuid,
    cancellation: &CancellationToken,
) -> Result<()> {
    let InstallInputs {
        prefix,
        runner,
        winebridge,
        environment,
    } = inputs;
    match step {
        InstallStep::Copy { destination, .. } if restore_files => {
            uninstall_file(prefix, destination).log_warn();
        }
        InstallStep::Copy { .. } => {}
        InstallStep::SetEnvironment { name, .. } => {
            environment.remove(name);
            shutdown_bridge(prefix).await.log_warn();
        }
        InstallStep::SetDllOverrides { dlls, .. } => {
            let command =
                WineBridgeClient::command(runner, prefix, winebridge).envs(environment.iter());
            let bridge = match WineBridgeClient::connect_or_spawn(prefix, command).await {
                Ok(bridge) => bridge,
                Err(error) => {
                    tracing::warn!(%error);
                    return Ok(());
                }
            };
            for dll in dlls.iter().rev() {
                check_cancellation(cancellation)?;
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
    check_cancellation(cancellation)
}

fn check_cancellation(cancellation: &CancellationToken) -> Result<()> {
    if cancellation.is_cancelled() {
        Err(Error::Cancelled)
    } else {
        Ok(())
    }
}

async fn wait_for_child(
    mut child: tokio::process::Child,
    cancellation: &CancellationToken,
) -> Result<std::process::ExitStatus> {
    tokio::select! {
        biased;
        status = child.wait() => Ok(status?),
        _ = cancellation.cancelled() => {
            child.kill().await?;
            Err(Error::Cancelled)
        }
    }
}

fn is_not_found(error: &Error) -> bool {
    matches!(error, Error::Status(status) if status.code() == tonic::Code::NotFound)
}

async fn shutdown_bridge(prefix: &Path) -> Result<()> {
    if let Some(bridge) = WineBridgeClient::try_connect(prefix).await? {
        bridge.shutdown().await?;
    }
    Ok(())
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
    let stage = prefix
        .parent()
        .expect("prefix has a parent")
        .join(".staging")
        .join(Uuid::new_v4().to_string());
    fs::create_dir_all(&stage)?;
    let result = (|| -> Result<()> {
        archive::extract(archive, &stage)?;
        for source in archive::files(&stage)? {
            let relative = destination.join(source.strip_prefix(&stage).map_err(|_| {
                InstallerError::FileOutsideStage {
                    path: source.clone(),
                    stage: stage.clone(),
                }
            })?);
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
