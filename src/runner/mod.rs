mod proton;
mod wine;

pub(crate) use crate::wrapper::{Command, Spawnable, Wrapper};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::error::Result;
use proton::Proton;
use wine::Wine;

use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
    process::ExitStatus,
};

#[derive(Debug, Clone, Copy, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum RunnerKind {
    Wine,
    Proton,
}

/// Errors produced by runner setup.
#[derive(Debug, Error)]
pub enum RunnerError {
    #[error("wineboot exited unsuccessfully: {0}")]
    WinebootFailed(ExitStatus),
    #[error("wineserver exited unsuccessfully: {0}")]
    WineserverFailed(ExitStatus),
    #[error("Proton runner requires an UMU executable")]
    UmuExecutableMissing,
    #[error("no supported runner executable was found in {0}")]
    RunnerNotFound(PathBuf),
    #[error("runner executable was not found: {0}")]
    RunnerExecutableNotFound(PathBuf),
}

#[derive(Debug)]
pub(crate) struct RunnerCommand(Command);

impl RunnerCommand {
    pub(crate) fn wrapped_by(self, wrapper: impl Wrapper) -> Self {
        Self(wrapper.wrap(self.0).into())
    }

    pub(crate) fn envs<K: AsRef<OsStr>, V: AsRef<OsStr>>(
        mut self,
        envs: impl IntoIterator<Item = (K, V)>,
    ) -> Self {
        self.0 = self.0.envs(envs);
        self
    }
}

impl From<RunnerCommand> for Command {
    fn from(command: RunnerCommand) -> Self {
        command.0
    }
}

impl Spawnable for RunnerCommand {}

#[async_trait]
pub(crate) trait Runner: Send + Sync {
    fn command(&self, prefix: &Path, inner: Command) -> RunnerCommand;

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

    async fn wineserver(&self, prefix: &Path, arg: &str) -> Result<()>;
}

pub(crate) async fn initialize_and_shutdown_prefix(
    runner: &dyn Runner,
    prefix: &Path,
) -> Result<()> {
    let initialized = runner.wineboot(prefix, "--init").await;
    let stopped = shutdown_prefix(runner, prefix).await;
    initialized?;
    stopped
}

pub(crate) async fn shutdown_prefix(runner: &dyn Runner, prefix: &Path) -> Result<()> {
    runner.wineserver(prefix, "-k").await
}

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

pub(crate) async fn load_runner(
    path: &Path,
    kind: RunnerKind,
    umu_root: Option<&Path>,
) -> Result<Box<dyn Runner>> {
    if detect_runner_kind(path).await? != kind {
        return Err(RunnerError::RunnerNotFound(path.to_path_buf()).into());
    }
    match kind {
        RunnerKind::Wine => Ok(Box::new(Wine::new(path.join("bin/wine")))),
        RunnerKind::Proton => {
            let umu = umu_root
                .ok_or(RunnerError::UmuExecutableMissing)?
                .join("umu-run");
            if !async_fs::metadata(&umu)
                .await
                .is_ok_and(|entry| entry.is_file())
            {
                return Err(RunnerError::RunnerExecutableNotFound(umu).into());
            }
            Ok(Box::new(Proton::new(path, umu)))
        }
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
