//! Addon installation recipes and their executor.
//!
//! Downloaded dependency artifacts retain their catalog recipes in the local
//! index. Components instead derive a built-in recipe from their [`super::Slot`],
//! allowing a bottle to remove a selected component without consulting the
//! catalog or local index.
//!
//! # Installation
//!
//! Resources and steps are applied in declaration order. Steps may copy or
//! extract files, run installers, register DLLs, update the registry, configure
//! DLL overrides, or change the bottle environment. Changes made by completed
//! steps remain if a later step fails; the bottle storage layer is responsible
//! for any transaction-level rollback.
//!
//! # Component removal
//!
//! Resources and steps are visited in reverse order. Uninstallation can restore
//! copied files, delete DLL overrides, and remove environment entries. Actions
//! without an inverse—executing programs, extracting archives, registering DLLs,
//! and setting registry values—are skipped. Consequently, a recipe is not
//! necessarily fully reversible. Dependencies cannot be removed separately from
//! their bottle.
//!
//! # Cancellation and cleanup
//!
//! Cancellation is cooperative. It is checked between steps and during
//! supported long-running work. Running child processes are killed and reaped
//! when possible; WineBridge calls already in flight are not interrupted.
//! Installation always attempts to stop WineBridge and the prefix runner before
//! returning.
//!
//! # Path handling
//!
//! Recipe paths are not checked for containment. Catalog data must therefore be
//! trusted.

mod engine;
mod recipes;

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{
    Directories,
    proto::{DllOverrideMode, RegistryHive, registry_value::Value as RegistryValue},
    runner::Runner,
    utils::environment::Environment,
};

use super::{Addon, Component, deserialize_non_empty_string};

pub(crate) use engine::{execute, replay_environment, uninstall};
pub(crate) use recipes::steps as recipe_steps;

/// One local resource and the installation steps applied to it.
///
/// Persisted dependency index entries store a single-component relative path.
/// Bottle installation resolves that path before passing the resource to the
/// engine. Component resources are derived directly from their slot and version.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct Artifact {
    pub(crate) path: PathBuf,
    pub(crate) steps: Vec<InstallStep>,
}

impl Artifact {
    pub(crate) fn new(path: PathBuf, steps: Vec<InstallStep>) -> Self {
        Self { path, steps }
    }
}

/// A declarative operation applied while installing an addon resource.
///
/// Steps are serialized as part of Bottles' internal catalog schema; their wire
/// representation is not a stable interchange API. The module overview describes
/// ordering, rollback, cancellation, and path requirements.
#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "action", rename_all = "kebab-case", deny_unknown_fields)]
pub(crate) enum InstallStep {
    /// Copies a resource file into the Wine prefix.
    ///
    /// An existing regular destination file is backed up once alongside the destination so an
    /// uninstall mode that restores files can reinstate it.
    Copy {
        /// Path intended to be relative to the resource, or empty to copy the resource itself.
        #[serde(default)]
        source: PathBuf,
        /// Destination intended to be relative to the Wine prefix.
        destination: PathBuf,
    },
    /// Runs the resource through the configured runner and requires a successful exit status.
    ///
    /// The process receives the bottle environment as it exists at this step.
    Execute {
        /// Passed directly to the child process without shell parsing.
        #[serde(default)]
        arguments: Vec<String>,
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
    /// Each process receives the bottle environment as it exists at this step.
    RegisterDlls {
        /// DLL paths intended to be relative to the Wine prefix.
        dlls: Vec<PathBuf>,
    },
    /// Sets a registry value through WineBridge.
    ///
    /// WineBridge is started with the current bottle environment when it is not
    /// already running.
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
    /// WineBridge is started with the current bottle environment when needed.
    /// Uninstall deletes these overrides rather than restoring their previous modes.
    SetDllOverrides {
        /// DLL names whose overrides are changed, in application order.
        dlls: Vec<String>,
        /// Applied uniformly; mixed per-DLL modes require separate steps.
        mode: DllOverrideMode,
    },
    /// Overwrites an entry in the bottle's process environment.
    ///
    /// The previous value is not retained. Uninstall removes the name rather than restoring a
    /// previous value, and WineBridge is stopped so a later operation starts it with the change.
    SetEnvironment { name: String, value: String },
}

/// Bottle-specific services and mutable state used while applying a recipe.
pub(crate) struct InstallInputs<'a> {
    /// The prepared Wine prefix receiving recipe changes.
    pub(crate) prefix: &'a Path,
    /// The runner used for Windows processes and prefix shutdown.
    pub(crate) runner: &'a dyn Runner,
    /// The WineBridge executable selected by the bottle.
    pub(crate) winebridge: &'a Path,
    /// The environment updated by `SetEnvironment` steps and passed to processes.
    pub(crate) environment: &'a mut Environment,
}

impl Addon<Component> {
    /// Derives the component resource and built-in recipe from stored metadata.
    pub(crate) fn artifact(&self, directories: &Directories) -> Artifact {
        Artifact::new(self.path(directories), recipe_steps(self.slot()).to_vec())
    }
}
