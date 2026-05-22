use std::{
    io::{self, ErrorKind},
    path::{Path, PathBuf},
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
    wine: Wine,
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
            wine,
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
        if prefix.compat_data_path.is_none() || prefix.compat_client_install_path.is_none() {
            return Err(RunnerError::ProtonEnvVarsMissing.into());
        }

        self.wine.run(prefix, command)
    }

    fn initialize_prefix(&self, prefix: &PrefixConfig) -> Result<()> {
        if prefix.compat_data_path.is_none() || prefix.compat_client_install_path.is_none() {
            return Err(RunnerError::ProtonEnvVarsMissing.into());
        }

        self.wine.initialize_prefix(prefix)
    }
}
