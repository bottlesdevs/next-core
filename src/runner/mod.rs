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

#[derive(Debug, Error)]
pub enum RunnerError {
    Command(#[from] RunnerCommandBuilderError),
    PrefixInitFailed,
}

impl std::fmt::Display for RunnerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RunnerError::Command(e) => write!(f, "{}", e),
            RunnerError::PrefixInitFailed => write!(f, "Failed to initialzie prefix using runner"),
        }
    }
}

/// Architecture for Wine prefix creation
///
/// Determines whether a Wine prefix should be configured for 32-bit or 64-bit
/// Windows compatibility. This affects which Windows applications can run
/// in the prefix
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrefixArch {
    /// 32-bit Windows prefix architecture
    Win32,
    /// 64-bit Windows prefix architecture (recommended)
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

pub struct PrefixConfig {
    path: PathBuf,
    arch: PrefixArch,
}

impl PrefixConfig {
    fn to_env(&self) -> HashMap<String, String> {
        let mut env = HashMap::new();

        env.insert(String::from("WINEPREFIX"), self.path.display().to_string());
        env.insert(String::from("WINEARCH"), self.arch.to_string());

        env
    }
}

#[derive(Builder, Clone)]
#[builder(pattern = "owned")]
pub struct RunnerCommand {
    #[builder(setter(custom))]
    executable: PathBuf,
    #[builder(field(ty = "Vec<String>"), setter(custom))]
    args: Vec<String>,
    #[builder(field(ty = "HashMap<String, String>"), setter(custom))]
    envs: HashMap<String, String>,
}

impl RunnerCommand {
    pub fn builder() -> RunnerCommandBuilder {
        RunnerCommandBuilder::default()
    }
}

impl RunnerCommandBuilder {
    pub fn executable(mut self, executable: impl AsRef<Path>) -> Self {
        self.executable = Some(executable.as_ref().to_path_buf());
        self
    }

    pub fn arg(mut self, arg: &str) -> Self {
        self.args.push(arg.to_string());
        self
    }

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

    pub fn env(mut self, key: &str, value: &str) -> Self {
        *self
            .envs
            .entry(key.to_string())
            .or_insert(value.to_string()) = value.to_string();
        self
    }

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
    fn run(&self, prefix: &PrefixConfig, command: RunnerCommand) -> Result<Child>;

    fn initialize_prefix(&self, prefix: &PrefixConfig) -> Result<()>;
}
