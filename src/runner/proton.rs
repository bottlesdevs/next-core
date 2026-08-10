//! UMU-backed Proton command lowering.
//!
//! Guest commands run through the paired UMU executable with `PROTONPATH` set to
//! the selected Proton directory, `WINEPREFIX` set to the bottle prefix, and
//! `WINEARCH=win64`. Server control also runs through UMU because Proton's
//! `wineserver` requires its runtime.

use async_trait::async_trait;
use std::path::{Path, PathBuf};

use super::{Command, Runner, RunnerCommand, RunnerError, Spawnable, Wrapper};
use crate::error::Result;

#[derive(Debug)]
pub(crate) struct Proton {
    proton_path: PathBuf,
    umu_executable: PathBuf,
}

impl Proton {
    pub fn new(proton_path: impl AsRef<Path>, umu_executable: impl AsRef<Path>) -> Self {
        Self {
            proton_path: proton_path.as_ref().to_path_buf(),
            umu_executable: umu_executable.as_ref().to_path_buf(),
        }
    }
}

#[async_trait]
impl Runner for Proton {
    fn command(&self, prefix: &Path, inner: Command) -> RunnerCommand {
        RunnerCommand(
            Command::new(&self.umu_executable)
                .env("WINEPREFIX", prefix)
                .env("WINEARCH", "win64")
                .env("PROTONPATH", &self.proton_path)
                .wrap(inner)
                .into(),
        )
    }

    /// Runs Proton's `wineserver` inside UMU's runtime.
    ///
    /// Exit status `1` is accepted only for `-k`; other commands and statuses
    /// retain normal success semantics.
    ///
    /// See <https://github.com/Open-Wine-Components/umu-launcher/issues/593>.
    async fn wineserver(&self, prefix: &Path, arg: &str) -> Result<()> {
        let command = Command::new(self.proton_path.join("files/bin/wineserver"))
            .arg(arg)
            .env("PROTONPATH", "umu-sniper");

        let status = self.command(prefix, command).spawn()?.status().await?;

        if status.success() || (arg == "-k" && status.code() == Some(1)) {
            return Ok(());
        }

        Err(RunnerError::WineserverFailed(status).into())
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::{fs, os::unix::fs::PermissionsExt};

    use super::*;

    #[test]
    fn all_proton_operations_run_through_umu_with_prefix_environment() {
        futures_lite::future::block_on(async {
            let root = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
            let proton_path = root.join("proton");
            let umu = root.join("umu-run");
            let log = root.join("umu.log");
            fs::create_dir_all(&proton_path).unwrap();
            fs::write(proton_path.join("proton"), []).unwrap();
            fs::write(
                &umu,
                format!(
                "#!/bin/sh\nlog='{}'\nprintf '%s|%s|%s|' \"$PROTONPATH\" \"$WINEPREFIX\" \"$WINEARCH\" >> \"$log\"\nprintf '<%s>' \"$@\" >> \"$log\"\nprintf '\\n' >> \"$log\"\n[ \"$2\" = -k ] && exit 1\n[ \"$2\" != --fail ]\n",
                log.display()
            ),
            )
            .unwrap();
            fs::set_permissions(&umu, fs::Permissions::from_mode(0o755)).unwrap();

            let runner = Proton::new(&proton_path, &umu);
            let prefix = root.join("prefix");
            runner.wineboot(&prefix, "--init").await.unwrap();
            runner
                .command(&prefix, Command::new("game.exe").arg("--flag"))
                .wrapped_by(Command::new("env"))
                .spawn()
                .unwrap()
                .status()
                .await
                .unwrap();
            runner.wineserver(&prefix, "-k").await.unwrap();
            assert!(matches!(
                runner.wineboot(&prefix, "--fail").await,
                Err(crate::error::Error::Runner(RunnerError::WinebootFailed(_)))
            ));
            assert!(matches!(
                runner.wineserver(&prefix, "--fail").await,
                Err(crate::error::Error::Runner(RunnerError::WineserverFailed(
                    _
                )))
            ));

            let environment = format!("{}|{}|win64|", proton_path.display(), prefix.display());
            let wineserver_environment = format!("umu-sniper|{}|win64|", prefix.display());
            assert_eq!(
                fs::read_to_string(&log).unwrap(),
                [
                    format!("{environment}<wineboot><--init>\n"),
                    format!("{environment}<game.exe><--flag>\n"),
                    format!(
                        "{wineserver_environment}<{}><-k>\n",
                        proton_path.join("files/bin/wineserver").display()
                    ),
                    format!("{environment}<wineboot><--fail>\n"),
                    format!(
                        "{wineserver_environment}<{}><--fail>\n",
                        proton_path.join("files/bin/wineserver").display()
                    ),
                ]
                .concat()
            );

            fs::remove_dir_all(root).unwrap();
        });
    }
}
