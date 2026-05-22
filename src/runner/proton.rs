use std::{
    io::{self, ErrorKind},
    path::{Path, PathBuf},
    process::Command,
};

use super::{PrefixConfig, Runner, RunnerCommand, RunnerError, Wine};
use crate::error::Result;

/// Proton runner implementation
///
/// Proton is Valve's Wine fork designed specifically for gaming on Linux. It includes
/// numerous patches and enhancements over standard Wine, making it particularly
/// effective for running Windows games through Steam or standalone.
///
/// # Note
/// When used outside of Steam, Proton requires specific environment variables:
/// - `STEAM_COMPAT_DATA_PATH`: Path to store compatibility data
/// - `STEAM_COMPAT_CLIENT_INSTALL_PATH`: Steam installation directory
#[derive(Debug)]
pub struct Proton {
    _wine: Wine,
    executable: PathBuf,
}

impl Proton {
    pub fn new(executable: impl AsRef<Path>) -> Result<Self> {
        if !executable.as_ref().exists() {
            return Err(io::Error::new(ErrorKind::NotFound, "Proton executable not found").into());
        }

        if !executable.as_ref().is_file() {
            return Err(
                io::Error::new(ErrorKind::InvalidInput, "Proton executable is not a file").into(),
            );
        }

        let parent_directory = executable
            .as_ref()
            .parent()
            .expect("Executable should have a parent directory");
        let wine = Wine::new(parent_directory.join("files/bin/wine"))?;

        Ok(Self {
            _wine: wine,
            executable: executable.as_ref().to_path_buf(),
        })
    }
}

impl Runner for Proton {
    fn run(
        &self,
        prefix: &PrefixConfig,
        command: RunnerCommand,
    ) -> crate::error::Result<std::process::Child> {
        if !prefix.is_proton() {
            return Err(RunnerError::ProtonEnvVarsMissing.into());
        }

        Command::new(&self.executable)
            .arg("run")
            .arg(command.executable)
            .args(command.args)
            .envs(prefix.to_env())
            .envs(command.envs)
            .spawn()
            .map_err(Into::into)
    }

    fn initialize_prefix(&self, prefix: &PrefixConfig) -> Result<()> {
        if !prefix.is_proton() {
            return Err(RunnerError::ProtonEnvVarsMissing.into());
        }

        // Safe to unwrap here since we already verified this is a proton prefix
        let compat_data_path = prefix.compat_data_path.as_deref().unwrap();
        if !compat_data_path.exists() {
            // Proton will fail to run compat_data_path doesn't exist, so create it
            std::fs::create_dir_all(compat_data_path)?;
        }

        let command = RunnerCommand::builder()
            .executable("wineboot")
            .arg("--init")
            .build()
            .map_err(Into::<RunnerError>::into)?;

        let status = self.run(prefix, command)?.wait()?;
        if !status.success() {
            return Err(RunnerError::PrefixInitFailed.into());
        }

        Ok(())
    }
}
