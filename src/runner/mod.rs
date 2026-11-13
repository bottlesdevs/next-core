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

use crate::{WineArch, error::Result};
pub(crate) use proton::Proton;
pub(crate) use wine::Wine;

use std::{
    path::{Path, PathBuf},
    process::ExitStatus,
};

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
    /// A selected runner or launcher path is not a regular executable file.
    #[error("runner executable was not found: {0}")]
    RunnerExecutableNotFound(PathBuf),
}

/// Wraps a host command that has already been lowered through a [`Runner`].
#[derive(Debug, derive_more::Into)]
pub(crate) struct RunnerCommand(Command);

impl RunnerCommand {
    /// Places one host wrapper around the lowered command.
    pub(crate) fn wrapped_by(self, wrapper: impl Wrapper) -> Self {
        Self(wrapper.wrap(self.0).into())
    }
}

impl Spawnable for RunnerCommand {}

/// Defines command lowering and Wine prefix lifecycle control for one runner kind.
#[async_trait]
pub(crate) trait Runner: Send + Sync {
    /// Lowers a guest `inner` command into a host command targeting `prefix`.
    fn command(&self, prefix: &Path, inner: Command) -> RunnerCommand;

    /// Runs `wineboot --init` to initialize the Wine prefix at `prefix` as `arch`.
    ///
    /// The architecture is passed only here: Wine records it when it creates a
    /// prefix and reads it from the prefix afterwards.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when the process cannot be spawned or waited for, or
    /// [`RunnerError::WinebootFailed`] when it exits unsuccessfully.
    async fn wineboot(&self, prefix: &Path, arch: WineArch) -> Result<()> {
        let RunnerCommand(command) = self.command(prefix, Command::new("wineboot").arg("--init"));
        let status = RunnerCommand(command.env("WINEARCH", <&str>::from(arch)))
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
