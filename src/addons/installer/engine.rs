//! Execution and reversal of addon installation recipes.

use std::{
    io,
    path::{Path, PathBuf},
};

use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{
    addons::InstallerError,
    error::{Error, Result},
    runner::{Command, Runner, Spawnable},
    utils::{archive, exists},
    winebridge::WineBridgeClient,
};

use super::{InstallInputs, InstallResource, InstallStep};

/// Applies every resource and step sequentially, reporting each step before it starts.
/// `backup_files` preserves displaced files for Standard removal; layered builds disable it.
///
/// Cancellation is checked before the first step, after every step, while waiting for child
/// processes, between per-DLL operations, and before and after extraction. Cancellation attempts to kill
/// and reap a running child; a kill failure is returned. The enclosing prefix scope stops Wine
/// before diffing, unmounting, or restoring storage.
pub(crate) async fn execute(
    inputs: InstallInputs<'_>,
    payload_root: &Path,
    resources: &[InstallResource],
    backup_files: bool,
    cancellation: &CancellationToken,
    on_step: impl Fn(&InstallStep) + Send,
) -> Result<()> {
    check_cancellation(cancellation)?;
    for resource in resources {
        let source = payload_root.join(&resource.path);
        for step in &resource.steps {
            on_step(step);
            execute_step(inputs, &source, step, backup_files, cancellation).await?;
            check_cancellation(cancellation)?;
        }
    }
    Ok(())
}

/// Attempts to undo a recipe in reverse resource and step order.
///
/// File copies are restored or removed and DLL overrides are deleted. Runtime variable
/// declarations and steps without an inverse are ignored. File, bridge and override
/// failures are returned.
/// The enclosing prefix scope owns Wine shutdown.
pub(crate) async fn uninstall<'a>(
    inputs: InstallInputs<'_>,
    steps: impl DoubleEndedIterator<Item = &'a InstallStep>,
    item_id: Uuid,
    cancellation: &CancellationToken,
    on_step: impl Fn(&InstallStep) + Send,
) -> Result<()> {
    check_cancellation(cancellation)?;
    for step in steps.rev() {
        on_step(step);
        uninstall_step(inputs, step, item_id, cancellation).await?;
        check_cancellation(cancellation)?;
    }
    Ok(())
}

async fn maintenance_bridge(
    runner: &dyn Runner,
    prefix: &Path,
    executable: &Path,
) -> Result<WineBridgeClient> {
    let command = WineBridgeClient::command(runner, prefix, executable, std::iter::empty());
    WineBridgeClient::connect_or_spawn(prefix, command).await
}

async fn execute_step(
    inputs: InstallInputs<'_>,
    resource: &Path,
    step: &InstallStep,
    backup_files: bool,
    cancellation: &CancellationToken,
) -> Result<()> {
    let InstallInputs {
        prefix,
        runner,
        winebridge,
    } = inputs;
    match step {
        InstallStep::SetEnvironment { .. } => {}
        InstallStep::Copy {
            source,
            destination,
        } => {
            let source = if source.as_os_str().is_empty() {
                resource.to_path_buf()
            } else {
                resource.join(source)
            };
            install_file(&source, prefix, destination, backup_files).await?;
        }
        InstallStep::Extract { destination } => {
            extract_into(resource, prefix, destination, backup_files, cancellation).await?;
        }
        InstallStep::Execute {
            arguments,
            env_vars,
        } => {
            let mut command = Command::new(resource);
            for argument in arguments {
                command = command.arg(argument);
            }
            for (name, value) in env_vars.iter() {
                command = command.env(name, value);
            }
            let status =
                wait_for_child(runner.command(prefix, command).spawn()?, cancellation).await?;
            if !status.success() {
                return Err(InstallerError::InstallerFailed(status).into());
            }
        }
        InstallStep::RegisterDlls { dlls, env_vars } => {
            for dll in dlls {
                check_cancellation(cancellation)?;
                let mut command = Command::new("regsvr32").arg("/s").arg(prefix.join(dll));
                for (name, value) in env_vars.iter() {
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
            let bridge = maintenance_bridge(runner, prefix, winebridge).await?;
            check_cancellation(cancellation)?;
            bridge
                .set_registry_value(*hive, key.clone(), name.clone(), value.clone())
                .await?;
        }
        InstallStep::SetDllOverrides { dlls, mode } => {
            let bridge = maintenance_bridge(runner, prefix, winebridge).await?;
            for dll in dlls {
                check_cancellation(cancellation)?;
                bridge.set_dll_override(dll.clone(), *mode).await?;
            }
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
    } = inputs;
    match step {
        InstallStep::SetEnvironment { .. } => {}
        InstallStep::Copy { destination, .. } => {
            uninstall_file(prefix, destination).await?;
        }
        InstallStep::SetDllOverrides { dlls, .. } => {
            let bridge = maintenance_bridge(runner, prefix, winebridge).await?;
            for dll in dlls.iter().rev() {
                check_cancellation(cancellation)?;
                if let Err(error) = bridge.delete_dll_override(dll.clone()).await {
                    if !is_not_found(&error) {
                        return Err(error);
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

/// Copies a file, optionally preserving the first displaced regular file for restoration.
///
/// The backup is stored alongside the destination with `.bak` appended. An existing backup is
/// never overwritten. `relative` is joined directly to `prefix` without containment validation.
///
/// # Panics
///
/// Panics if the resulting destination has no parent directory.
async fn install_file(
    source: &Path,
    prefix: &Path,
    relative: &Path,
    backup_files: bool,
) -> Result<()> {
    let destination = prefix.join(relative);
    async_fs::create_dir_all(destination.parent().expect("destination has a parent")).await?;
    let relative_backup = backup_path(relative);
    let backup = prefix.join(&relative_backup);
    if backup_files
        && async_fs::metadata(&destination)
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
    match async_fs::metadata(&backup).await {
        Ok(entry) if entry.is_file() => {
            async_fs::copy(&backup, &destination).await?;
            async_fs::remove_file(backup).await
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            match async_fs::remove_file(destination).await {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(error),
            }
        }
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("backup is not a regular file: {}", backup.display()),
        )),
        Err(error) => Err(error),
    }
}

/// Extracts an archive into an isolated staging directory, then installs its files.
///
/// Files are installed in sorted path order through [`install_file`], following the caller's
/// backup policy. The staging directory is removed on a best-effort basis regardless
/// of the operation's result; a cleanup error does not replace the extraction result.
///
/// # Panics
///
/// Panics if `prefix` has no parent directory.
async fn extract_into(
    archive: &Path,
    prefix: &Path,
    destination: &Path,
    backup_files: bool,
    cancellation: &CancellationToken,
) -> Result<()> {
    let stage = prefix
        .parent()
        .expect("prefix has a parent")
        .join(".staging")
        .join(Uuid::new_v4().to_string());
    async_fs::create_dir_all(&stage).await?;
    let work = async {
        check_cancellation(cancellation)?;
        archive::extract(archive, &stage).await?;
        check_cancellation(cancellation)?;
        for source in archive::files(&stage).await? {
            check_cancellation(cancellation)?;
            let relative = destination.join(source.strip_prefix(&stage).map_err(|_| {
                InstallerError::FileOutsideStage {
                    path: source.clone(),
                    stage: stage.clone(),
                }
            })?);
            install_file(&source, prefix, &relative, backup_files).await?;
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
                    true,
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
