mod proton;
mod wine;

use derive_builder::Builder;
use thiserror::Error;
pub use wine::Wine;

use crate::error::Result;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Child,
};

/// Errors produced by runner command construction and prefix setup.
#[derive(Debug, Error)]
pub enum RunnerError {
    #[error("RunnerCommand could not be built from the provided builder fields: {0}")]
    Command(#[from] RunnerCommandBuilderError),
    #[error("The runner process used for prefix initialization exited unsuccessfully.")]
    PrefixInitFailed,
    #[error(
        "Proton runner requires STEAM_COMPAT_DATA_PATH and STEAM_COMPAT_CLIENT_INSTALL_PATH environment variables to be set in the prefix configuration."
    )]
    ProtonEnvVarsMissing,
}

/// Architecture for Wine prefix creation
///
/// Determines whether a Wine prefix should be configured for 32-bit or 64-bit
/// Windows compatibility. This affects which Windows applications can run
/// in the prefix
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PrefixArch {
    /// 32-bit Windows prefix architecture
    Win32,
    /// 64-bit Windows prefix architecture (recommended)
    #[default]
    Win64,
}

/// Windows version compatibility settings
///
/// Specifies which version of Windows the Wine prefix should emulate.
/// Different applications may require specific Windows versions for
/// optimal compatibility.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowsVersion {
    Win7,
    Win8,
    Win10,
}

impl std::fmt::Display for PrefixArch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let str = match self {
            PrefixArch::Win32 => "win32".to_string(),
            PrefixArch::Win64 => "win64".to_string(),
        };

        write!(f, "{}", str)
    }
}

/// Configuration used when creating or running commands inside a Wine prefix.
///
/// Runners use this value to set process-level environment such as `WINEPREFIX` and
/// `WINEARCH` before invoking Wine, Proton, UMU, or GPTK.
#[derive(Default, Debug)]
pub struct PrefixConfig {
    path: PathBuf,
    arch: PrefixArch,
    compat_data_path: Option<PathBuf>,
    compat_client_install_path: Option<PathBuf>,
}

impl PrefixConfig {
    /// Creates a prefix configuration for `path` and `arch`.
    ///
    /// This does not create the prefix on disk. Use [`Runner::initialize_prefix`]
    /// to ask a runner to initialize the configured prefix.
    pub fn new(path: impl AsRef<Path>, arch: PrefixArch) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
            arch,
            ..Default::default()
        }
    }

    /// Converts the prefix configuration into runner process environment values.
    ///
    /// The returned map currently contains `WINEPREFIX` and `WINEARCH`.
    pub fn to_env(&self) -> HashMap<String, String> {
        let mut env = HashMap::new();

        env.insert(String::from("WINEPREFIX"), self.path.display().to_string());
        env.insert(String::from("WINEARCH"), self.arch.to_string());

        if let Some(path) = &self.compat_data_path {
            env.insert(
                String::from("STEAM_COMPAT_DATA_PATH"),
                path.display().to_string(),
            );
        }

        if let Some(path) = &self.compat_client_install_path {
            env.insert(
                String::from("STEAM_COMPAT_CLIENT_INSTALL_PATH"),
                path.display().to_string(),
            );
        }

        env
    }
}

/// Command description passed to a [`Runner`].
///
/// `RunnerCommand` describes what should be executed by a compatibility runner.
#[derive(Builder, Clone)]
#[builder(pattern = "owned")]
pub struct RunnerCommand {
    /// Executable or runner subcommand to invoke.
    #[builder(setter(custom))]
    executable: PathBuf,

    /// Positional arguments passed after the executable.
    #[builder(field(ty = "Vec<String>"), setter(custom))]
    args: Vec<String>,

    /// Environment variables applied to the runner process.
    #[builder(field(ty = "HashMap<String, String>"), setter(custom))]
    envs: HashMap<String, String>,
}

impl RunnerCommand {
    /// Creates a builder for a runner command.
    pub fn builder() -> RunnerCommandBuilder {
        RunnerCommandBuilder::default()
    }
}

impl RunnerCommandBuilder {
    /// Sets the executable or runner subcommand to invoke.
    ///
    /// For Wine this is usually a Windows executable path or a Wine subcommand
    /// e.g `wineboot`.
    pub fn executable(mut self, executable: impl AsRef<Path>) -> Self {
        self.executable = Some(executable.as_ref().to_path_buf());
        self
    }

    /// Appends one positional argument.
    pub fn arg(mut self, arg: &str) -> Self {
        self.args.push(arg.to_string());
        self
    }

    /// Appends multiple positional arguments in order.
    pub fn args<I, A>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = A>,
        A: AsRef<str>,
    {
        for arg in args {
            self = self.arg(arg.as_ref());
        }

        self
    }

    /// Sets or replaces one environment variable for the runner process.
    pub fn env(mut self, key: &str, value: &str) -> Self {
        *self
            .envs
            .entry(key.to_string())
            .or_insert(value.to_string()) = value.to_string();
        self
    }

    /// Sets or replaces multiple environment variables for the runner process.
    pub fn envs<I, K, V>(mut self, envs: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: AsRef<str>,
        V: AsRef<str>,
    {
        for (key, val) in envs {
            self = self.env(key.as_ref(), val.as_ref());
        }
        self
    }
}

/// Trait defining the common interface for all Windows compatibility runners
///
/// All runners in this module implement this trait, providing a unified way to interact
/// with different compatibility layers like Wine, Proton, UMU, and GPTK.
pub trait Runner {
    /// Starts a runner command inside `prefix`.
    ///
    /// Implementations should translate [`RunnerCommand`] into the correct host
    /// process invocation,  apply command environment overrides,
    /// and return the spawned child process.
    fn run(&self, prefix: &PrefixConfig, command: RunnerCommand) -> Result<Child>;

    /// Initializes the configured prefix on disk using the runner.
    ///
    /// This is host-side setup and should complete before a WineBridge server is
    /// started for the prefix.
    fn initialize_prefix(&self, prefix: &PrefixConfig) -> Result<()>;
}
