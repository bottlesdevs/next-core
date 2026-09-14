//! Frozen recipe data shared by catalog acquisition and addon execution.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{
    proto::{DllOverrideMode, RegistryHive, registry_value::Value as RegistryValue},
    utils::env_vars::EnvVars,
};

use super::deserialize_non_empty_string;

/// A local installation resource and its frozen recipe.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct InstallResource {
    /// Relative to the release payload; empty for a component's payload directory.
    pub(crate) path: PathBuf,
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

/// An installation action or runtime environment declaration for an addon resource.
///
/// Steps are serialized as part of Bottles' internal catalog schema; their wire
/// representation is not a stable interchange API. The installer module describes
/// ordering, rollback, cancellation, and path requirements.
#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "action", rename_all = "kebab-case", deny_unknown_fields)]
pub(crate) enum InstallStep {
    /// Copies a resource file into the Wine prefix.
    ///
    /// Standard backs up an existing regular file once for restoration during uninstall.
    /// Shared layered builds disable these backups; their lower layer retains the original.
    Copy {
        /// Path intended to be relative to the resource, or empty to copy the resource itself.
        #[serde(default)]
        source: PathBuf,
        /// Destination intended to be relative to the Wine prefix.
        destination: PathBuf,
    },
    /// Runs the resource through the configured runner and requires a successful exit status.
    ///
    /// Variables apply only to this command, in addition to the host environment.
    Execute {
        /// Passed directly to the child process without shell parsing.
        #[serde(default)]
        arguments: Vec<String>,
        #[serde(default, skip_serializing_if = "EnvVars::is_empty")]
        env_vars: EnvVars,
    },
    /// Extracts a supported tar archive and copies its regular files into the Wine prefix.
    ///
    /// Extraction uses a temporary staging directory. Archive links and special entries are
    /// rejected, and removal of the staging directory is attempted after success, failure, or
    /// cancellation. Extracted files are copied sequentially, temporarily requiring space for
    /// both the staged and installed copies.
    Extract {
        /// Destination intended to be relative to the Wine prefix.
        destination: PathBuf,
    },
    /// Registers DLLs silently with `regsvr32` in list order.
    ///
    /// Variables apply only to these commands, in addition to the host environment.
    RegisterDlls {
        /// DLL paths intended to be relative to the Wine prefix.
        dlls: Vec<PathBuf>,
        #[serde(default, skip_serializing_if = "EnvVars::is_empty")]
        env_vars: EnvVars,
    },
    /// Sets a registry value through WineBridge.
    ///
    /// WineBridge is started with the runner's maintenance environment when needed.
    SetRegistryValue {
        hive: RegistryHive,
        /// Non-empty registry key path.
        #[serde(deserialize_with = "deserialize_non_empty_string")]
        key: String,
        /// Value name within the key; an empty name addresses the default value.
        name: String,
        value: RegistryValue,
    },
    /// Applies the same Wine DLL override mode to each named DLL.
    ///
    /// WineBridge is started with the runner's maintenance environment when needed.
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
    SetEnvironment { name: String, value: String },
}
