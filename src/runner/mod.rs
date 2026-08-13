//! Runner discovery, command lowering, and prefix lifecycle control.
//!
//! A [`Runner`] converts a Windows command into the host process that executes
//! it for one prefix. [`RunnerCommand`] marks that this lowering has happened so
//! host wrappers can be added without bypassing runner-specific environment.

mod gptk;
mod proton;
mod wine;

pub(crate) use crate::wrapper::{Command, Spawnable, Wrapper};
use async_trait::async_trait;
use thiserror::Error;

use crate::error::Result;
pub(crate) use gptk::Gptk;
pub(crate) use proton::Proton;
pub(crate) use wine::Wine;

use std::{
    ffi::OsStr,
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
    /// A Game Porting Toolkit layout selected by `bin/wine64`; server control
    /// expects its sibling `wineserver`.
    Gptk,
}

/// A Wine release, as reported by a runner's own executable.
///
/// Only the leading `major.minor` of strings such as `wine-11.0` or
/// `wine-7.7 (Game Porting Toolkit 1.1)` is retained, which is enough to gate
/// features on a minimum release. Ordering is by major then minor.
#[derive(Debug, Clone, Copy, Eq, Ord, PartialEq, PartialOrd)]
pub struct WineVersion {
    pub major: u32,
    pub minor: u32,
}

impl WineVersion {
    /// Parses the leading version of a `--version` line, ignoring any suffix
    /// such as a distribution name in parentheses.
    pub fn parse(reported: &str) -> Option<Self> {
        let reported = reported.trim();
        let numbers = reported.strip_prefix("wine-").unwrap_or(reported);
        let numbers = numbers.split_whitespace().next()?;
        let mut parts = numbers.split('.');
        Some(Self {
            major: parts.next()?.parse().ok()?,
            minor: parts
                .next()
                .map(|minor| minor.trim_end_matches(|c: char| !c.is_ascii_digit()))
                .and_then(|minor| minor.parse().ok())
                .unwrap_or(0),
        })
    }
}

impl std::fmt::Display for WineVersion {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "wine-{}.{}", self.major, self.minor)
    }
}

/// Reports the Wine version of an executable that answers `--version`.
async fn report_version(executable: &Path) -> Option<WineVersion> {
    let output = async_process::Command::new(executable)
        .arg("--version")
        .output()
        .await
        .ok()?;
    WineVersion::parse(&String::from_utf8_lossy(&output.stdout))
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

    /// Reports the runner's Wine release, when this layout can be asked for one.
    ///
    /// Used to gate features that older Wine cannot support. [`None`] means the
    /// version is unknown and no such gate applies, so a layout that does not
    /// answer `--version` is never rejected for its version alone.
    async fn wine_version(&self) -> Option<WineVersion> {
        None
    }
}

/// Initializes a prefix and then attempts to stop its server.
///
/// Shutdown is attempted even when initialization fails. If both fail, the
/// initialization error takes precedence.
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

/// Classifies a component by its regular-file markers.
///
/// `proton` takes precedence over `bin/wine`, which takes precedence over
/// `bin/wine64`, when more than one exists. Missing markers and marker
/// metadata failures are reported as [`RunnerError::RunnerNotFound`].
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
    } else if async_fs::metadata(path.join("bin/wine64"))
        .await
        .is_ok_and(|entry| entry.is_file())
    {
        Ok(RunnerKind::Gptk)
    } else {
        Err(RunnerError::RunnerNotFound(path.to_path_buf()).into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wrapper::{Wrappers, gamescope::GamescopeConfig, mangohud::MangoHudConfig};

    #[test]
    fn parses_reported_wine_versions() {
        // The two strings that matter in practice: a stock Wine build, and GPTK,
        // which appends its own product name.
        assert_eq!(
            WineVersion::parse("wine-11.0"),
            Some(WineVersion {
                major: 11,
                minor: 0
            })
        );
        assert_eq!(
            WineVersion::parse("wine-7.7 (Game Porting Toolkit 1.1)\n"),
            Some(WineVersion { major: 7, minor: 7 })
        );
        assert_eq!(
            WineVersion::parse("wine-11.15"),
            Some(WineVersion {
                major: 11,
                minor: 15
            })
        );
        assert_eq!(
            WineVersion::parse("wine-9.0-rc1"),
            Some(WineVersion { major: 9, minor: 0 })
        );
        assert_eq!(WineVersion::parse("not a version"), None);
    }

    /// The bridge gate is `>= 7.13`; 7.12 and older lack `IOCTL_AFD_POLL` on a
    /// standalone AFD handle.
    #[test]
    fn orders_versions_across_the_bridge_minimum() {
        let minimum = crate::winebridge::MINIMUM_WINE;
        assert_eq!(minimum.to_string(), "wine-7.13");
        for older in ["wine-7.7", "wine-7.12", "wine-6.23"] {
            assert!(WineVersion::parse(older).unwrap() < minimum, "{older}");
        }
        for newer in ["wine-7.13", "wine-7.22", "wine-8.0", "wine-11.0"] {
            assert!(WineVersion::parse(newer).unwrap() >= minimum, "{newer}");
        }
    }

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
