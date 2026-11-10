//! Materializes software directly into a conventional Wine prefix.
//!
//! Creation initializes Wine and leaves no processes running. Later software
//! edits uninstall removed or replaced prefix components before installing
//! replacements and newly appended dependencies in one maintenance session.

use super::super::runtime;
use crate::{
    Job, PrefixState, Slot,
    addons::{InstallInputs, execute, uninstall},
    error::Result,
};
use std::path::Path;
use strum::IntoEnumIterator;

/// Creates the prefix directory, initializes Wine, and stops its processes.
///
/// # Errors
///
/// Returns an error if directory creation, runner loading, Wine initialization,
/// or shutdown fails. A shutdown failure retains the prefix.
pub(in crate::prefix) async fn create(config: &PrefixState, root: &Path) -> Result<()> {
    let wineprefix = root.join("prefix");
    async_fs::create_dir_all(&wineprefix).await?;
    let runner = config.runner.load_runner(config.umu.as_ref()).await?;
    runtime::initialize(runner.as_ref(), &wineprefix).await
}

/// Applies the difference between two frozen selections to a stopped prefix.
///
/// Removed prefix components use the previous selection's embedded recipe and prefix
/// backups. Replacements and newly appended dependencies read their payloads as
/// each recipe runs, so a late failure can leave earlier steps applied. The
/// caller must stop the prefix first; this function performs one final shutdown
/// after the batch whether application succeeds or fails.
/// `candidate.dependencies` must begin with exactly `previous.dependencies`;
/// edits enforce this by exposing append-only dependencies.
///
/// # Errors
///
/// Returns an error if runner loading, uninstalling, installing, cancellation,
/// or the final Wine shutdown fails. A shutdown error takes precedence over the
/// application result.
///
/// # Panics
///
/// Panics if `candidate.dependencies` is shorter than `previous.dependencies`.
/// Edits preserve the previous dependencies, so this indicates an internal
/// caller violated the edit contract.
pub(in crate::prefix) async fn apply(
    previous: &PrefixState,
    candidate: &PrefixState,
    root: &Path,
    job: &Job,
) -> Result<()> {
    let mut removals = Vec::new();
    let mut components = Vec::new();
    let dependencies = &candidate.dependencies[previous.dependencies.len()..];
    for slot in Slot::iter() {
        let old = previous.component(slot);
        let new = candidate.component(slot);
        if old == new {
            continue;
        }
        if let Some(old) = old {
            removals.push(old);
        }
        if let Some(new) = new {
            components.push(new);
        }
    }
    let runner = candidate.runner.load_runner(candidate.umu.as_ref()).await?;
    let wineprefix = root.join("prefix");
    let winebridge = candidate.winebridge.path();
    let inputs = InstallInputs {
        wineprefix: &wineprefix,
        runner: runner.as_ref(),
        winebridge: &winebridge,
    };
    let applied = async {
        for release in removals {
            uninstall(inputs, release.recipe(), release.id(), job).await?;
        }
        let installations = components
            .iter()
            .map(|r| (r.path(), r.resources()))
            .chain(dependencies.iter().map(|r| (r.path(), r.resources())));
        for (payload, resources) in installations {
            execute(inputs, &payload, resources, true, job).await?;
        }
        Ok(())
    }
    .await;
    runtime::stop(runner.as_ref(), &wineprefix).await?;
    applied
}
