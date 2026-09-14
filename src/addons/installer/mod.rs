//! Execution and reversal of frozen addon recipes.
//!
//! Each local release stores its installation recipes together with its source payload.
//! Built-in recipes supply component defaults during import and download.
//!
//! # Installation
//!
//! Resources and steps are applied in declaration order. Steps may copy or
//! extract files, run installers, register DLLs, update the registry, or configure
//! DLL overrides. `SetEnvironment` declarations are collected into release metadata
//! during acquisition and ignored during installation and uninstall. Installer
//! commands declare their own variables. Changes made by completed steps remain
//! if a later step fails; the bottle storage layer is responsible
//! for any transaction-level rollback.
//!
//! # Component removal
//!
//! Resources and steps are visited in reverse order. Uninstallation can restore
//! copied files and delete DLL overrides. Actions without an inverse—executing
//! programs, extracting archives, registering DLLs,
//! and setting registry values—are skipped. Failures while reversing supported
//! steps are returned. Dependencies cannot be removed separately from their bottle.
//!
//! # Cancellation and cleanup
//!
//! Cancellation is cooperative. It is checked between steps and during
//! supported long-running work. Running child processes are killed and reaped
//! when possible; WineBridge calls already in flight are not interrupted.
//! The enclosing prefix scope stops WineBridge and the prefix runner before
//! releasing storage.
//!
//! # Path handling
//!
//! Recipe paths are not checked for containment. Catalog data must therefore be
//! trusted.

mod engine;

use std::path::Path;

use crate::runner::Runner;

pub(crate) use engine::{execute, uninstall};

/// Execution inputs for a recipe in an owner prefix or shared build.
#[derive(Clone, Copy)]
pub(crate) struct InstallInputs<'a> {
    /// The prepared Wine prefix receiving recipe changes.
    pub(crate) prefix: &'a Path,
    /// The runner used for Windows processes. The execution workflow owns shutdown.
    pub(crate) runner: &'a dyn Runner,
    /// The WineBridge executable selected by the execution workflow.
    pub(crate) winebridge: &'a Path,
}
