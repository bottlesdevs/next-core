//! Execution and reversal of addon installation recipes.

use std::{
    io,
    path::{Path, PathBuf},
};

use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{
    addons::InstallerError,
    error::{Error, Result, ResultExt},
    runner::{Command, Runner, Spawnable},
    utils::{archive, env_vars::EnvVars, exists},
    winebridge::WineBridgeClient,
};

use super::{Artifact, InstallInputs, InstallStep};

/// Applies every resource and step sequentially, reporting each step before it starts.
///
/// Cancellation is checked before the first step, after every step, while waiting for child
/// processes, between per-DLL operations, and during extraction. Cancellation attempts to kill
/// and reap a running child; a kill failure is returned. The enclosing prefix scope stops Wine
/// before diffing, unmounting, or restoring storage.
pub(crate) async fn execute(
    inputs: InstallInputs<'_>,
    resources: &[Artifact],
    cancellation: &CancellationToken,
    on_step: impl Fn(&InstallStep) + Send,
) -> Result<()> {
    let InstallInputs {
        prefix,
        runner,
        winebridge,
        env_vars,
        explicit_env_vars,
    } = inputs;
    check_cancellation(cancellation)?;
    for resource in resources {
        for step in &resource.steps {
            on_step(step);
            execute_step(
                InstallInputs {
                    prefix,
                    runner,
                    winebridge,
                    env_vars: &mut *env_vars,
                    explicit_env_vars,
                },
                resource,
                step,
                cancellation,
            )
            .await?;
            check_cancellation(cancellation)?;
        }
    }
    Ok(())
}

/// Attempts to undo a recipe in reverse resource and step order.
///
/// File copies are restored or removed. Environment entries are
/// removed and DLL overrides are deleted. Other step kinds have no inverse and are skipped with a
/// warning. File, bridge and override failures are logged and ignored; cancellation is returned.
/// The enclosing prefix scope owns Wine shutdown.
pub(crate) async fn uninstall(
    inputs: InstallInputs<'_>,
    resources: &[Artifact],
    item_id: Uuid,
    cancellation: &CancellationToken,
    on_step: impl Fn(&InstallStep) + Send,
) -> Result<()> {
    let InstallInputs {
        prefix,
        runner,
        winebridge,
        env_vars,
        explicit_env_vars,
    } = inputs;

    check_cancellation(cancellation)?;
    for resource in resources.iter().rev() {
        for step in resource.steps.iter().rev() {
            on_step(step);
            uninstall_step(
                InstallInputs {
                    prefix,
                    runner,
                    winebridge,
                    env_vars: &mut *env_vars,
                    explicit_env_vars,
                },
                step,
                item_id,
                cancellation,
            )
            .await?;
            check_cancellation(cancellation)?;
        }
    }
    Ok(())
}

/// Collects recipe variables in declaration order; later values override earlier ones.
pub(crate) fn replay_env_vars<'a>(
    env_vars: &mut EnvVars,
    steps: impl IntoIterator<Item = &'a InstallStep>,
) {
    for step in steps {
        if let InstallStep::SetEnvironment { name, value } = step {
            env_vars.insert(name.clone(), value.clone());
        }
    }
}

async fn maintenance_bridge(
    runner: &dyn Runner,
    prefix: &Path,
    executable: &Path,
    env_vars: &EnvVars,
    explicit_env_vars: &EnvVars,
) -> Result<WineBridgeClient> {
    let command = WineBridgeClient::command(
        runner,
        prefix,
        executable,
        env_vars.iter().chain(explicit_env_vars.iter()),
    );
    WineBridgeClient::connect_or_spawn(prefix, command).await
}

async fn execute_step(
    inputs: InstallInputs<'_>,
    resource: &Artifact,
    step: &InstallStep,
    cancellation: &CancellationToken,
) -> Result<()> {
    let InstallInputs {
        prefix,
        runner,
        winebridge,
        env_vars,
        explicit_env_vars,
    } = inputs;
    match step {
        InstallStep::Copy {
            source,
            destination,
        } => {
            let source = if source.as_os_str().is_empty() {
                resource.path.clone()
            } else {
                resource.path.join(source)
            };
            install_file(&source, prefix, destination).await?;
        }
        InstallStep::Extract { destination } => {
            extract_into(&resource.path, prefix, destination, cancellation).await?;
        }
        InstallStep::Execute { arguments } => {
            let mut command = Command::new(&resource.path);
            for argument in arguments {
                command = command.arg(argument);
            }
            for (name, value) in env_vars.iter().chain(explicit_env_vars.iter()) {
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
                for (name, value) in env_vars.iter().chain(explicit_env_vars.iter()) {
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
            let bridge =
                maintenance_bridge(runner, prefix, winebridge, env_vars, explicit_env_vars).await?;
            check_cancellation(cancellation)?;
            bridge
                .set_registry_value(*hive, key.clone(), name.clone(), value.clone())
                .await?;
        }
        InstallStep::SetDllOverrides { dlls, mode } => {
            let bridge =
                maintenance_bridge(runner, prefix, winebridge, env_vars, explicit_env_vars).await?;
            for dll in dlls {
                check_cancellation(cancellation)?;
                bridge.set_dll_override(dll.clone(), *mode).await?;
            }
        }
        InstallStep::SetEnvironment { name, value } => {
            env_vars.insert(name.clone(), value.clone());
            WineBridgeClient::shutdown_existing(prefix).await?;
        }
    }
    Ok(())
}

async fn uninstall_step(
    inputs: InstallInputs<'_>,
    step: &InstallStep,
    addon_id: Uuid,
    cancellation: &CancellationToken,
) -> Result<()> {
    let InstallInputs {
        prefix,
        runner,
        winebridge,
        env_vars,
        explicit_env_vars,
    } = inputs;
    match step {
        InstallStep::Copy { destination, .. } => {
            if let Err(error) = uninstall_file(prefix, destination).await {
                tracing::warn!(%error);
            }
        }
        InstallStep::SetEnvironment { name, .. } => {
            env_vars.remove(name);
            WineBridgeClient::shutdown_existing(prefix).await.log_warn();
        }
        InstallStep::SetDllOverrides { dlls, .. } => {
            let bridge =
                match maintenance_bridge(runner, prefix, winebridge, env_vars, explicit_env_vars)
                    .await
                {
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
                %addon_id,
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

/// Waits for a child to exit, or attempts to kill and reap it when cancellation wins the race.
///
/// An already-exited child may reject the kill with [`io::ErrorKind::InvalidInput`]; this is
/// ignored before the child is reaped and cancellation is returned. Other kill failures are
/// returned without another reap attempt.
async fn wait_for_child(
    mut child: async_process::Child,
    cancellation: &CancellationToken,
) -> Result<std::process::ExitStatus> {
    let status = cancellation.run_until_cancelled(child.status()).await;
    if let Some(status) = status {
        return Ok(status?);
    }
    if let Err(error) = child.kill()
        && error.kind() != io::ErrorKind::InvalidInput
    {
        return Err(error.into());
    }
    child.status().await?;
    Err(Error::Cancelled)
}

fn is_not_found(error: &Error) -> bool {
    matches!(error, Error::Status(status) if status.code() == tonic::Code::NotFound)
}

/// Copies a file into a prefix, preserving the first displaced regular file as a backup.
///
/// The backup is stored alongside the destination with `.bak` appended. An existing backup is
/// never overwritten. `relative` is joined directly to `prefix` without containment validation.
///
/// # Panics
///
/// Panics if the resulting destination has no parent directory.
async fn install_file(source: &Path, prefix: &Path, relative: &Path) -> Result<()> {
    let destination = prefix.join(relative);
    async_fs::create_dir_all(destination.parent().expect("destination has a parent")).await?;
    let relative_backup = backup_path(relative);
    let backup = prefix.join(&relative_backup);
    if async_fs::metadata(&destination)
        .await
        .is_ok_and(|entry| entry.is_file())
        && !exists(&backup).await?
    {
        async_fs::copy(&destination, &backup).await?;
    }
    async_fs::copy(source, destination).await?;
    Ok(())
}

/// Restores a copied file's backup, or removes the installed file when no backup exists.
///
/// A restored backup is deleted after it is copied. A missing installed file is treated as an
/// already-completed uninstall.
async fn uninstall_file(prefix: &Path, relative: &Path) -> io::Result<()> {
    let destination = prefix.join(relative);
    let backup = prefix.join(backup_path(relative));
    if async_fs::metadata(&backup)
        .await
        .is_ok_and(|entry| entry.is_file())
    {
        async_fs::copy(&backup, &destination).await?;
        async_fs::remove_file(backup).await
    } else {
        match async_fs::remove_file(destination).await {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }
}

/// Extracts an archive into an isolated staging directory, then installs its files.
///
/// Files are installed in sorted path order through [`install_file`], preserving displaced files
/// for possible restoration. The staging directory is removed on a best-effort basis regardless
/// of the operation's result; a cleanup error does not replace the extraction result.
///
/// # Panics
///
/// Panics if `prefix` has no parent directory.
async fn extract_into(
    archive: &Path,
    prefix: &Path,
    destination: &Path,
    cancellation: &CancellationToken,
) -> Result<()> {
    let stage = prefix
        .parent()
        .expect("prefix has a parent")
        .join(".staging")
        .join(Uuid::new_v4().to_string());
    async_fs::create_dir_all(&stage).await?;
    let work = async {
        cancellation
            .run_until_cancelled(archive::extract(archive, &stage))
            .await
            .ok_or(Error::Cancelled)??;
        for source in archive::files(&stage).await? {
            check_cancellation(cancellation)?;
            let relative = destination.join(source.strip_prefix(&stage).map_err(|_| {
                InstallerError::FileOutsideStage {
                    path: source.clone(),
                    stage: stage.clone(),
                }
            })?);
            install_file(&source, prefix, &relative).await?;
        }
        check_cancellation(cancellation)?;
        Ok::<_, Error>(())
    };
    let result = work.await;
    let _ = async_fs::remove_dir_all(stage).await;
    result
}

fn backup_path(path: &Path) -> PathBuf {
    let mut path = path.as_os_str().to_os_string();
    path.push(".bak");
    PathBuf::from(path)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn cancellation_kills_and_reaps_child() {
        futures_lite::future::block_on(async {
            let child = async_process::Command::new("sh")
                .args(["-c", "sleep 30"])
                .spawn()
                .unwrap();
            let cancellation = CancellationToken::new();
            cancellation.cancel();

            assert!(matches!(
                wait_for_child(child, &cancellation).await,
                Err(Error::Cancelled)
            ));
        });
    }

    #[test]
    fn cancelled_extraction_skips_archive_and_removes_stage() {
        futures_lite::future::block_on(async {
            let root =
                std::env::temp_dir().join(format!("bottles-next-installer-{}", Uuid::new_v4()));
            std::fs::create_dir_all(&root).unwrap();
            let cancellation = CancellationToken::new();
            cancellation.cancel();

            assert!(matches!(
                extract_into(
                    &root.join("missing.tar"),
                    &root.join("prefix"),
                    Path::new("drive_c"),
                    &cancellation,
                )
                .await,
                Err(Error::Cancelled)
            ));
            assert!(
                std::fs::read_dir(root.join(".staging"))
                    .unwrap()
                    .next()
                    .is_none()
            );

            std::fs::remove_dir_all(root).unwrap();
        });
    }
}
