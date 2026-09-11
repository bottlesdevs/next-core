//! Runner discovery, command lowering, and prefix lifecycle control.
//!
//! A [`Runner`] converts a Windows command into the host process that executes
//! it for one prefix. [`RunnerCommand`] marks that this lowering has happened so
//! host wrappers can be added without bypassing runner-specific environment.

mod proton;
mod wine;

pub(crate) use crate::wrapper::{Command, Spawnable, Wrapper};
use async_trait::async_trait;
use thiserror::Error;

use crate::error::Result;
pub(crate) use proton::Proton;
pub(crate) use wine::Wine;

use std::{
    path::{Path, PathBuf},
    process::ExitStatus,
};

/// The launch protocol and installed layout of a runner component.
///
/// This identifies how next-core invokes the component, not a distribution or
/// version of Wine.
#[derive(Debug, Clone, Copy, Eq, Hash, PartialEq)]
pub(crate) enum RunnerKind {
    /// A direct Wine layout selected by `bin/wine`; server control expects its
    /// sibling `wineserver`.
    Wine,
    /// A Proton layout launched through a separately managed UMU executable.
    Proton,
}

/// Failures while discovering or controlling a runner.
///
/// Process variants retain unsuccessful exit statuses. Failures to spawn or
/// wait for those processes are reported as [`crate::error::Error::Io`] instead.
#[derive(Debug, Error)]
pub enum RunnerError {
    #[error("wineboot exited unsuccessfully: {0}")]
    WinebootFailed(ExitStatus),
    #[error("wineserver exited unsuccessfully: {0}")]
    WineserverFailed(ExitStatus),
    /// No UMU component was paired with the selected Proton component.
    #[error("Proton runner requires an UMU executable")]
    UmuExecutableMissing,
    /// The component layout was unsupported or disagreed with its recorded kind.
    #[error("no supported runner executable was found in {0}")]
    RunnerNotFound(PathBuf),
    /// A paired component did not contain its expected regular executable file.
    #[error("runner executable was not found: {0}")]
    RunnerExecutableNotFound(PathBuf),
}

/// A host command that has been lowered through a [`Runner`].
#[derive(Debug)]
pub(crate) struct RunnerCommand(Command);

impl RunnerCommand {
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

/// Runner-specific command construction and prefix lifecycle operations.
#[async_trait]
pub(crate) trait Runner: Send + Sync {
    /// Lowers a Windows command into a host command targeting `prefix`.
    fn command(&self, prefix: &Path, inner: Command) -> RunnerCommand;

    /// Runs `wineboot` through this runner and requires a successful exit status.
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

    /// Runs runner-specific server control, including any status normalization.
    async fn wineserver(&self, prefix: &Path, arg: &str) -> Result<()>;
}

/// Classifies a component by its regular-file markers.
///
/// `proton` takes precedence over `bin/wine` when both exist. Missing markers
/// and marker metadata failures are reported as [`RunnerError::RunnerNotFound`].
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
    use crate::wrapper::{Wrappers, gamescope::GamescopeConfig, mangohud::MangoHudConfig};

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
