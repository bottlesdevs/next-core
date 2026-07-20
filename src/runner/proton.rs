use std::{
    path::{Path, PathBuf},
    process::{Child, Command},
};

use super::{Runner, RunnerCommand, RunnerError};
use crate::error::Result;

/// Proton runner implementation
///
/// Proton is Valve's Wine fork designed specifically for gaming on Linux. It includes
/// numerous patches and enhancements over standard Wine, making it particularly
/// effective for running Windows games through Steam or standalone.
///
#[derive(Debug)]
pub struct Proton {
    proton_path: PathBuf,
    umu_executable: PathBuf,
}

impl Proton {
    pub fn new(proton_path: impl AsRef<Path>, umu_executable: impl AsRef<Path>) -> Result<Self> {
        if !proton_path.as_ref().join("proton").is_file() {
            return Err(
                RunnerError::RunnerExecutableNotFound(proton_path.as_ref().join("proton")).into(),
            );
        }

        if !umu_executable.as_ref().is_file() {
            return Err(RunnerError::RunnerExecutableNotFound(
                umu_executable.as_ref().to_path_buf(),
            )
            .into());
        }

        Ok(Self {
            proton_path: proton_path.as_ref().to_path_buf(),
            umu_executable: umu_executable.as_ref().to_path_buf(),
        })
    }
}

impl Runner for Proton {
    fn run(&self, prefix: &Path, command: RunnerCommand) -> Result<Child> {
        Command::new(&self.umu_executable)
            .arg(command.executable)
            .args(command.args)
            .env("WINEPREFIX", prefix)
            .env("WINEARCH", "win64")
            .env("PROTONPATH", &self.proton_path)
            .envs(command.envs)
            .spawn()
            .map_err(Into::into)
    }

    // See: https://github.com/Open-Wine-Components/umu-launcher/issues/593#issuecomment-3958136985
    fn wineserver(&self, prefix: &Path, arg: &str) -> Result<()> {
        let command = RunnerCommand::new(self.proton_path.join("files/bin/wineserver"))
            .arg(arg)
            .env("PROTONPATH", "umu-sniper");
        let status = self.run(prefix, command)?.wait()?;
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

        let runner = Proton::new(&proton_path, &umu).unwrap();
        let prefix = root.join("prefix");
        runner.wineboot(&prefix, "--init").unwrap();
        runner
            .run(&prefix, RunnerCommand::new("game.exe").arg("--flag"))
            .unwrap()
            .wait()
            .unwrap();
        runner.wineserver(&prefix, "-k").unwrap();
        assert!(matches!(
            runner.wineboot(&prefix, "--fail"),
            Err(crate::error::Error::Runner(RunnerError::WinebootFailed(_)))
        ));
        assert!(matches!(
            runner.wineserver(&prefix, "--fail"),
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
    }
}
