//! Execution and partial reversal of frozen addon recipes.
//!
//! Installation walks resources and steps in declaration order. Removal walks the
//! saved steps in reverse, restoring copied files and deleting DLL overrides; steps
//! without a defined inverse are logged and skipped. Completed changes are not
//! rolled back if a later step fails.
//!
//! Cancellation is cooperative between steps and during child processes and archive
//! work. Recipe paths are joined directly to payload and prefix roots, so recipes
//! must originate from trusted catalog data.

use std::{
    io,
    path::{Path, PathBuf},
};

use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{
    addons::InstallerError,
    command::{Command, Spawnable},
    error::{Error, Result},
    runner::Runner,
    utils::fs::{self, archive, exists},
    winebridge::WineBridgeClient,
};

use crate::addons::recipe::{InstallResource, InstallStep};

/// Groups the runtime services and directories needed to apply a recipe.
#[derive(Clone, Copy)]
pub(crate) struct InstallInputs<'a> {
    /// Wine prefix that receives recipe changes.
    pub(crate) prefix: &'a Path,
    /// Parent directory for temporary archive extraction.
    pub(crate) staging: &'a Path,
    /// Runner used for Windows child processes.
    pub(crate) runner: &'a dyn Runner,
    /// Root of the WineBridge component used for registry operations.
    pub(crate) winebridge: &'a Path,
}

/// Applies all resource steps in declaration order.
///
/// `on_step` runs immediately before each step. If `backup_files` is `true`, a
/// displaced regular file is preserved once for later removal; layered builds pass
/// `false` because their lower layer retains the original.
///
/// # Errors
///
/// Returns an error if cancellation is requested or any filesystem, archive,
/// process, runner, or WineBridge operation fails. An unsuccessful installer or
/// DLL-registration child is reported as [`InstallerError`]. Earlier steps are not
/// rolled back.
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

/// Reverses supported steps in reverse recipe order.
///
/// Copied files are restored from backups or removed, and DLL overrides are deleted.
/// Environment declarations are ignored; executable, extraction, DLL-registration,
/// and registry-write steps are logged and skipped because they have no inverse.
///
/// # Errors
///
/// Returns an error if cancellation is requested or restoring files or deleting DLL
/// overrides fails. Changes reversed before the failure remain reversed.
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
        staging,
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
            extract_into(
                resource,
                prefix,
                staging,
                destination,
                backup_files,
                cancellation,
            )
            .await?;
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
        ..
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
                bridge.delete_dll_override(dll.clone()).await?;
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

/// Waits for a child to exit, killing and reaping it if cancellation wins.
///
/// [`io::ErrorKind::InvalidInput`] from killing an already-exited child is ignored;
/// the child is still reaped before cancellation is returned.
///
/// # Errors
///
/// Returns an error if waiting for or killing the child fails, or returns
/// [`Error::Cancelled`] after cancellation and successful cleanup.
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

/// Copies `source` into the prefix and optionally preserves the displaced file.
///
/// The backup is stored beside the destination with `.bak` appended and is never
/// overwritten. `relative` is joined directly to `prefix` without containment
/// validation.
///
/// # Errors
///
/// Returns an error if destination directories cannot be created, metadata cannot
/// be inspected, or a copy fails.
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

/// Restores a copied file's backup or removes the installed file.
///
/// A restored backup is removed after it has been copied over the destination.
///
/// # Errors
///
/// Returns an error if backup inspection, restoration, or removal fails.
async fn uninstall_file(prefix: &Path, relative: &Path) -> io::Result<()> {
    let destination = prefix.join(relative);
    let backup = prefix.join(backup_path(relative));
    match async_fs::metadata(&backup).await {
        Ok(_) => {
            async_fs::copy(&backup, &destination).await?;
            async_fs::remove_file(backup).await
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            async_fs::remove_file(destination).await
        }
        Err(error) => Err(error),
    }
}

/// Extracts an archive into temporary storage and installs its regular files.
///
/// Files are copied in sorted path order through [`install_file`] using the caller's
/// backup policy. Temporary storage is cleaned up on a best-effort basis.
///
/// # Errors
///
/// Returns an error on cancellation, unsafe or unsupported archive contents, or a
/// staging or installation filesystem failure.
async fn extract_into(
    archive: &Path,
    prefix: &Path,
    staging: &Path,
    destination: &Path,
    backup_files: bool,
    cancellation: &CancellationToken,
) -> Result<()> {
    fs::with_temp_dir(staging, |stage| async move {
        check_cancellation(cancellation)?;
        archive::extract(archive, &stage).await?;
        check_cancellation(cancellation)?;
        for source in archive::files(&stage).await? {
            check_cancellation(cancellation)?;
            let relative = destination.join(source.strip_prefix(&stage).unwrap());
            install_file(&source, prefix, &relative, backup_files).await?;
        }
        check_cancellation(cancellation)?;
        Ok::<_, Error>(())
    })
    .await
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
                    &root.join(".staging"),
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
