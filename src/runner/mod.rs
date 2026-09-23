//! Adapts Wine-compatible runtimes to a common command and lifecycle interface.
//!
//! A [`Runner`] lowers guest commands into the host process required by either a
//! direct Wine installation or UMU-backed Proton. [`RunnerCommand`] marks the
//! lowered form so host wrappers can be composed without skipping runner-specific
//! arguments or environment variables.

mod proton;
mod wine;

use crate::command::{Command, Spawnable, Wrapper};
use async_trait::async_trait;
use thiserror::Error;

use crate::error::Result;
pub(crate) use proton::Proton;
pub(crate) use wine::Wine;

use std::{
    path::{Path, PathBuf},
    process::ExitStatus,
};

/// Classifies the invocation protocol implied by an installed runner layout.
///
/// The value identifies how next-core invokes a component, not its Wine
/// distribution or version.
#[derive(Debug, Clone, Copy, Eq, Hash, PartialEq)]
pub(crate) enum RunnerKind {
    /// Uses `bin/wine` directly and its sibling `bin/wineserver` for server control.
    Wine,
    /// Uses a root-level `proton` marker and launches through a paired UMU executable.
    Proton,
}

/// Describes runner discovery, configuration, and process failures.
///
/// Process variants retain unsuccessful exit statuses. Failures to spawn or
/// wait for those processes are reported as [`crate::error::Error::Io`] instead.
#[derive(Debug, Error)]
pub enum RunnerError {
    /// `wineboot` exited with a non-success status.
    #[error("wineboot exited unsuccessfully: {0}")]
    WinebootFailed(ExitStatus),
    /// `wineserver` exited with a status not accepted by the selected runner.
    #[error("wineserver exited unsuccessfully: {0}")]
    WineserverFailed(ExitStatus),
    /// A Proton component was selected without a paired UMU component.
    #[error("Proton runner requires an UMU executable")]
    UmuExecutableMissing,
    /// The component directory contains no supported runner marker.
    #[error("no supported runner executable was found in {0}")]
    RunnerNotFound(PathBuf),
    /// A selected runner or launcher path is not a regular executable file.
    #[error("runner executable was not found: {0}")]
    RunnerExecutableNotFound(PathBuf),
}

/// Wraps a host command that has already been lowered through a [`Runner`].
#[derive(Debug)]
pub(crate) struct RunnerCommand(Command);

impl RunnerCommand {
    /// Places one host wrapper around the lowered command.
    pub(crate) fn wrapped_by(self, wrapper: impl Wrapper) -> Self {
        Self(wrapper.wrap(self.0).into())
    }
}

impl From<RunnerCommand> for Command {
    fn from(command: RunnerCommand) -> Self {
        command.0
    }
}

impl Spawnable for RunnerCommand {}

/// Defines command lowering and Wine prefix lifecycle control for one runner kind.
#[async_trait]
pub(crate) trait Runner: Send + Sync {
    /// Lowers a guest `inner` command into a host command targeting `prefix`.
    fn command(&self, prefix: &Path, inner: Command) -> RunnerCommand;

    /// Runs `wineboot` through this runner and requires a successful exit status.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when the process cannot be spawned or waited for, or
    /// [`RunnerError::WinebootFailed`] when it exits unsuccessfully.
    async fn wineboot(&self, prefix: &Path, arg: &str) -> Result<()> {
        let status = self
            .command(prefix, Command::new("wineboot").arg(arg))
            .spawn()?
            .status()
            .await?;

        if !status.success() {
            return Err(RunnerError::WinebootFailed(status).into());
        }

        Ok(())
    }

    /// Runs runner-specific server control, including accepted status normalization.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when the process cannot be spawned or waited for, or
    /// [`RunnerError::WineserverFailed`] for an unaccepted exit status.
    async fn wineserver(&self, prefix: &Path, arg: &str) -> Result<()>;
}

/// Classifies an installed component by its regular-file markers.
///
/// A root-level `proton` marker takes precedence over `bin/wine` when both exist.
///
/// # Errors
///
/// Returns [`RunnerError::RunnerNotFound`] when neither marker is a regular file.
/// Metadata failures are treated the same as missing or non-file markers.
pub(crate) async fn detect_runner_kind(path: &Path) -> Result<RunnerKind> {
    if async_fs::metadata(path.join("proton"))
        .await
        .is_ok_and(|entry| entry.is_file())
    {
        Ok(RunnerKind::Proton)
    } else if async_fs::metadata(path.join("bin/wine"))
        .await
        .is_ok_and(|entry| entry.is_file())
    {
        Ok(RunnerKind::Wine)
    } else {
        Err(RunnerError::RunnerNotFound(path.to_path_buf()).into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::wrappers::{
        Wrappers, gamescope::GamescopeConfig, mangohud::MangoHudConfig,
    };

    #[test]
    fn configured_wrappers_lower_valid_combinations() {
        fn command(executable: &str, args: &[&str]) -> Command {
            Command::new(executable).args(args.iter().copied())
        }

        for (gamescope, mangohud, expected) in [
            (false, false, command("wine", &["bridge.exe"])),
            (
                false,
                true,
                command("mangohud", &["--", "wine", "bridge.exe"]),
            ),
            (
                true,
                false,
                command("gamescope", &["--", "wine", "bridge.exe"]),
            ),
            (
                true,
                true,
                command("gamescope", &["--mangoapp", "--", "wine", "bridge.exe"]),
            ),
        ] {
            let command = Wrappers {
                gamescope: GamescopeConfig {
                    enabled: gamescope,
                    ..Default::default()
                },
                mangohud: MangoHudConfig { enabled: mangohud },
            }
            .apply(RunnerCommand(Command::new("wine").arg("bridge.exe")));

            assert_eq!(Command::from(command), expected);
        }
    }
}
