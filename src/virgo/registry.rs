//! Captures, diffs, and composes Wine registry state for Virgo layers.
//!
//! Artifact layers keep registry changes separate from filesystem revisions.
//! Composition applies selected artifact patches to the immutable base and then
//! replays the workspace's private changes, preserving user state across selection
//! changes without allowing overlay precedence to hide the managed hives.

use super::VirgoError;
use crate::{
    error::{Error, Result},
    utils::fs::with_temp_dir,
};
use regdiff_rs::prelude::{Diff, Hive, Registry, apply_files};
use std::{
    fs,
    path::{Path, PathBuf},
};

/// Returns the managed Wine registry filenames and their semantic hives.
pub(super) fn registry_files() -> [(&'static str, Hive); 2] {
    [
        ("user.reg", Hive::CurrentUser),
        ("system.reg", Hive::LocalMachine),
    ]
}

/// Serializes the forward registry difference from `old` to `new`.
///
/// # Errors
///
/// Returns [`VirgoError::Registry`] if either hive cannot be parsed or the
/// resulting difference cannot be serialized.
fn write_forward(old: &Path, new: &Path, output: &Path, hive: Hive) -> Result<()> {
    let old =
        Registry::try_from(old, hive).map_err(|error| VirgoError::Registry(error.to_string()))?;
    let new =
        Registry::try_from(new, hive).map_err(|error| VirgoError::Registry(error.to_string()))?;
    Registry::diff(&old, &new)
        .serialize_file(output)
        .map_err(|error| VirgoError::Registry(error.to_string()))?;
    Ok(())
}

/// Copies initial hives from a stopped base prefix into artifact storage.
///
/// # Errors
///
/// Returns an I/O error if the destination cannot be created or either managed
/// hive cannot be copied.
pub(super) async fn capture(prefix: &Path, before: &Path) -> Result<()> {
    async_fs::create_dir_all(before).await?;
    for (file, _) in registry_files() {
        async_fs::copy(prefix.join(file), before.join(file)).await?;
    }
    Ok(())
}

/// Writes forward patches for both managed hives after Wine has stopped.
///
/// Empty differences are still serialized so every published artifact has a
/// complete registry representation.
///
/// # Errors
///
/// Returns an I/O or [`VirgoError::Registry`] error while reading, diffing, or
/// serializing either hive.
pub(super) async fn write_patches(before: &Path, prefix: &Path, patches: &Path) -> Result<()> {
    let (before, prefix, patches) = (
        before.to_path_buf(),
        prefix.to_path_buf(),
        patches.to_path_buf(),
    );
    blocking::unblock(move || {
        fs::create_dir_all(&patches)?;
        for (file, hive) in registry_files() {
            write_forward(
                &before.join(file),
                &prefix.join(file),
                &patches.join(file),
                hive,
            )?;
        }
        Ok(())
    })
    .await
}

/// Removes managed hive files from a layer's filesystem before committing it.
///
/// Missing hive files are accepted because registry effects are carried separately.
///
/// # Errors
///
/// Returns an I/O error other than a missing file while removing a hive.
pub(super) async fn exclude_hives(filesystem: &Path) -> Result<()> {
    for (file, _) in registry_files() {
        match async_fs::remove_file(filesystem.join(file)).await {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

/// Replays private registry changes from the prior baseline onto a new baseline.
///
/// # Errors
///
/// Returns an I/O or [`VirgoError::Registry`] error while producing either hive.
fn merge_private(previous: &Path, upper: &Path, baseline: &Path, merged: &Path) -> Result<()> {
    fs::create_dir_all(merged)?;
    for (file, hive) in registry_files() {
        if previous.is_dir() {
            let patch = merged.join(format!("{file}.patch"));
            let current = upper.join(file);
            let empty = merged.join("empty.reg");
            if !current.is_file() {
                fs::write(&empty, "WINE REGISTRY Version 2\n\n")?;
            }
            write_forward(
                &previous.join(file),
                if current.is_file() { &current } else { &empty },
                &patch,
                hive,
            )?;
            apply_files(baseline.join(file), patch.clone(), merged.join(file), hive)
                .map_err(|error| VirgoError::Registry(error.to_string()))?;
            fs::remove_file(patch)?;
            if empty.exists() {
                fs::remove_file(empty)?;
            }
        } else {
            fs::copy(baseline.join(file), merged.join(file))?;
        }
    }
    Ok(())
}

/// Replaces managed hives with a newly composed baseline plus private changes.
///
/// The caller must provide a stopped, checkpointed workspace. Artifact `patches`
/// are applied to `initial` in order, then changes made privately since the prior
/// baseline are replayed. Other upper files and whiteouts retain normal overlay
/// precedence.
///
/// # Errors
///
/// Returns an I/O or [`VirgoError::Registry`] error while copying, parsing,
/// diffing, applying, or replacing registry files.
pub(super) async fn compose(
    root: &Path,
    staging: &Path,
    initial: &Path,
    patches: Vec<PathBuf>,
) -> Result<()> {
    let root = root.to_path_buf();
    let initial = initial.to_path_buf();
    with_temp_dir(staging, |scratch| {
        blocking::unblock(move || {
            let baseline = scratch.join("baseline");
            fs::create_dir_all(&baseline)?;
            for (file, _) in registry_files() {
                fs::copy(initial.join(file), baseline.join(file))?;
            }
            for patch in patches {
                for (file, hive) in registry_files() {
                    let path = baseline.join(file);
                    apply_files(&path, &patch.join(file), &path, hive)
                        .map_err(|error| VirgoError::Registry(error.to_string()))?;
                }
            }
            let previous = root.join("registry-baseline");
            let upper = root.join("upper");
            let merged = scratch.join("merged");
            merge_private(&previous, &upper, &baseline, &merged)?;
            for (file, _) in registry_files() {
                fs::rename(merged.join(file), upper.join(file))?;
                let whiteout = upper.join(format!(".wh.{file}"));
                if whiteout.exists() {
                    fs::remove_file(whiteout)?;
                }
            }
            if previous.exists() {
                fs::remove_dir_all(&previous)?;
            }
            fs::rename(baseline, previous)?;
            Ok::<_, Error>(())
        })
    })
    .await
}
