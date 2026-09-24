//! Serializable resources and actions that make up a frozen addon recipe.
//!
//! Acquisition resolves catalog defaults into these records. Execution later uses
//! the stored recipe rather than consulting the current catalog.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{
    proto::{DllOverrideMode, RegistryHive, registry_value::Value as RegistryValue},
    utils::env_vars::EnvVars,
};

/// Associates one payload path with the steps that consume it.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct InstallResource {
    /// Path relative to the release payload, or empty for the component payload root.
    pub(crate) path: PathBuf,
    /// Actions applied to this resource in declaration order.
    pub(crate) steps: Vec<InstallStep>,
}

impl InstallResource {
    pub(crate) fn new(path: impl Into<PathBuf>, steps: Vec<InstallStep>) -> Self {
        Self {
            path: path.into(),
            steps,
        }
    }
}

/// Describes one installation action or runtime environment declaration.
///
/// Actions run in declaration order during installation and, where supported, in
/// reverse order during removal. Paths are joined to trusted payload and prefix
/// roots without containment validation; catalogs are therefore trusted input.
#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "action", rename_all = "kebab-case", deny_unknown_fields)]
pub(crate) enum InstallStep {
    /// Copies a payload file into the Wine prefix.
    ///
    /// Standard prefixes can preserve an existing regular file for removal. Layered
    /// builds rely on their lower layer instead.
    Copy {
        /// Path relative to the resource, or empty to copy the resource itself.
        #[serde(default)]
        source: PathBuf,
        /// Destination relative to the Wine prefix.
        destination: PathBuf,
    },
    /// Runs the resource through the configured runner.
    ///
    /// Variables apply only to this command, in addition to the host environment.
    Execute {
        /// Arguments passed directly to the child process without shell parsing.
        #[serde(default)]
        arguments: Vec<String>,
        /// Environment variables scoped to this child process.
        #[serde(default, skip_serializing_if = "EnvVars::is_empty")]
        env_vars: EnvVars,
    },
    /// Extracts a tar archive and copies its regular files into the Wine prefix.
    ///
    /// Extraction uses a temporary staging directory. Archive links and special entries are
    /// rejected, and removal of the staging directory is attempted after success, failure, or
    /// cancellation. Extracted files are copied sequentially, temporarily requiring space for
    /// both the staged and installed copies.
    Extract {
        /// Destination relative to the Wine prefix.
        destination: PathBuf,
    },
    /// Registers DLLs silently with `regsvr32` in list order.
    ///
    /// Variables apply only to these commands, in addition to the host environment.
    RegisterDlls {
        /// DLL paths relative to the Wine prefix, processed in declaration order.
        dlls: Vec<PathBuf>,
        /// Environment variables scoped to each `regsvr32` child process.
        #[serde(default, skip_serializing_if = "EnvVars::is_empty")]
        env_vars: EnvVars,
    },
    /// Sets a registry value through `WineBridge`.
    ///
    /// `WineBridge` is started with the runner's maintenance environment when needed.
    SetRegistryValue {
        /// Root registry hive containing the target key.
        hive: RegistryHive,
        /// Registry key path passed to `WineBridge`.
        key: String,
        /// Value name within the key; an empty name addresses the default value.
        name: String,
        /// Registry value data written at `name`.
        value: RegistryValue,
    },
    /// Applies the same Wine DLL override mode to each named DLL.
    ///
    /// `WineBridge` is started with the runner's maintenance environment when needed.
    /// Uninstall deletes these overrides rather than restoring their previous modes.
    SetDllOverrides {
        /// DLL names whose overrides are changed, in application order.
        dlls: Vec<String>,
        /// Applied uniformly; mixed per-DLL modes require separate steps.
        mode: DllOverrideMode,
    },
    /// Declares a launch variable derived from the frozen recipe at runtime.
    ///
    /// Later declarations win. Installation and uninstall ignore this declaration;
    /// installer commands use their own explicit variables.
    SetEnvironment {
        /// Environment variable name.
        name: String,
        /// Environment variable value.
        value: String,
    },
}
