//! Execution and partial reversal of frozen addon recipes.
//!
//! Installation walks resources and steps in declaration order. Removal walks the
//! saved steps in reverse, restoring copied files; steps without an inverse are
//! logged and skipped. Completed changes are not rolled back if a later step fails.
//!
//! Cancellation is cooperative between steps and while child processes run. Archive
//! extraction and individual file copies are not interrupted; cancellation is
//! observed around them. Recipe paths are joined directly to payload and prefix
//! roots, so recipes must originate from trusted catalog data.

use std::{
    io,
    path::{Path, PathBuf},
};

use uuid::Uuid;

use crate::{
    Job, Stage,
    addons::InstallerError,
    command::{Command, Spawnable},
    error::{Error, Result},
    runner::Runner,
    utils::fs::{self, archive, exists},
};

use crate::addons::recipe::{InstallResource, InstallStep};

/// Groups the runtime services and locations needed to apply a recipe.
#[derive(Clone, Copy)]
pub(crate) struct InstallInputs<'a> {
    /// Wine prefix that receives recipe changes.
    pub(crate) wineprefix: &'a Path,
    /// Runner used for Windows child processes.
    pub(crate) runner: &'a dyn Runner,
}

/// Applies all resource steps in declaration order.
///
/// Reports [`Stage::Configuring`] before each step. If `backup_files` is `true`, a
/// displaced regular file is preserved once for later removal; layered builds pass
/// `false` because their lower layer retains the original.
///
/// # Errors
///
/// Returns an error if cancellation is requested or any filesystem, archive,
/// process, or runner operation fails. An unsuccessful installer or
/// DLL-registration child is reported as [`InstallerError`]. Earlier steps are not
/// rolled back.
pub(crate) async fn execute(
    inputs: InstallInputs<'_>,
    payload_root: &Path,
    resources: &[InstallResource],
    backup_files: bool,
    job: &Job,
) -> Result<()> {
    for resource in resources {
        let source = payload_root.join(&resource.path);
        for step in &resource.steps {
            job.transition(Stage::Configuring)?;
            execute_step(inputs, &source, step, backup_files, job).await?;
        }
    }
    Ok(())
}

/// Reverses supported steps in reverse recipe order.
///
/// Copied files are restored from backups or removed. Executable, extraction,
/// and DLL-registration steps are logged and skipped because they have no inverse.
///
/// # Errors
///
/// Returns an error if cancellation is requested or restoring files fails.
/// Changes reversed before the failure remain reversed.
pub(crate) async fn uninstall<'a>(
    inputs: InstallInputs<'_>,
    steps: impl DoubleEndedIterator<Item = &'a InstallStep>,
    item_id: Uuid,
    job: &Job,
) -> Result<()> {
    for step in steps.rev() {
        job.transition(Stage::Removing)?;
        uninstall_step(inputs, step, item_id).await?;
    }
    Ok(())
}

async fn execute_step(
    inputs: InstallInputs<'_>,
    resource: &Path,
    step: &InstallStep,
    backup_files: bool,
    job: &Job,
) -> Result<()> {
    let InstallInputs { wineprefix, runner } = inputs;
    match step {
        InstallStep::Copy {
            source,
            destination,
        } => {
            let source = if source.as_os_str().is_empty() {
                resource.to_path_buf()
            } else {
                resource.join(source)
            };
            install_file(&source, wineprefix, destination, backup_files).await?;
        }
        InstallStep::Extract { destination } => {
            extract_into(resource, wineprefix, destination, backup_files, job).await?;
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
            let status = wait_for_child(runner.command(wineprefix, command).spawn()?, job).await?;
            if !status.success() {
                return Err(InstallerError::InstallerFailed(status).into());
            }
        }
        InstallStep::RegisterDlls { dlls, env_vars } => {
            for dll in dlls {
                let mut command = Command::new("regsvr32").arg("/s").arg(wineprefix.join(dll));
                for (name, value) in env_vars.iter() {
                    command = command.env(name, value);
                }
                let status =
                    wait_for_child(runner.command(wineprefix, command).spawn()?, job).await?;
                if !status.success() {
                    return Err(InstallerError::RegisterDllFailed(status).into());
                }
            }
        }
    }
    Ok(())
}

async fn uninstall_step(
    inputs: InstallInputs<'_>,
    step: &InstallStep,
    addon_id: Uuid,
) -> Result<()> {
    match step {
        InstallStep::Copy { destination, .. } => {
            uninstall_file(inputs.wineprefix, destination).await?;
        }
        unsupported => {
            tracing::warn!(
                %addon_id,
                step = ?unsupported,
                "skipping unsupported component uninstall action"
            );
        }
    }
    Ok(())
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
    job: &Job,
) -> Result<std::process::ExitStatus> {
    let cancelled = match job.cancellable(child.status()).await {
        Ok(status) => return Ok(status?),
        Err(cancelled) => cancelled,
    };
    if let Err(error) = child.kill()
        && error.kind() != io::ErrorKind::InvalidInput
    {
        return Err(error.into());
    }
    child.status().await?;
    Err(cancelled)
}

/// Copies `source` into the prefix and optionally preserves the displaced file.
///
/// The backup is stored beside the destination with `.bak` appended and is never
/// overwritten. With backups enabled, an existing destination is preserved only
/// when metadata successfully identifies it as a regular file; a metadata error
/// skips backup and the install copy is still attempted. `relative` is joined
/// directly to `wineprefix` without containment validation.
///
/// # Errors
///
/// Returns an error if destination directories cannot be created, backup existence
/// cannot be checked, or a copy fails.
///
/// # Panics
///
/// Panics if the resulting destination has no parent directory.
async fn install_file(
    source: &Path,
    wineprefix: &Path,
    relative: &Path,
    backup_files: bool,
) -> Result<()> {
    let destination = wineprefix.join(relative);
    async_fs::create_dir_all(destination.parent().expect("destination has a parent")).await?;
    let relative_backup = backup_path(relative);
    let backup = wineprefix.join(&relative_backup);
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
async fn uninstall_file(wineprefix: &Path, relative: &Path) -> io::Result<()> {
    let destination = wineprefix.join(relative);
    let backup = wineprefix.join(backup_path(relative));
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
    wineprefix: &Path,
    destination: &Path,
    backup_files: bool,
    job: &Job,
) -> Result<()> {
    fs::with_staging(|stage| async move {
        archive::extract(archive, &stage).await?;
        for source in archive::files(&stage).await? {
            job.check_cancellation()?;
            let relative = destination.join(source.strip_prefix(&stage).unwrap());
            install_file(&source, wineprefix, &relative, backup_files).await?;
        }
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
    use crate::Operation;
    use tokio::sync::oneshot;

    #[test]
    fn cancellation_kills_and_reaps_child() {
        futures_lite::future::block_on(async {
            let child = async_process::Command::new("sh")
                .args(["-c", "sleep 30"])
                .spawn()
                .unwrap();
            let (token_tx, token_rx) = oneshot::channel();
            let operation = Operation::new(|job| async move {
                let token: tokio_util::sync::CancellationToken = token_rx.await.unwrap();
                token.cancel();
                wait_for_child(child, &job).await
            });
            token_tx.send(operation.cancellation_token()).unwrap();

            assert!(matches!(operation.await, Err(Error::Cancelled)));
        });
    }
}
