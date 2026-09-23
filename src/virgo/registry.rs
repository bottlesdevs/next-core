//! Compose the managed registry baseline, then replay private changes over it.

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

pub(super) fn registry_files() -> [(&'static str, Hive); 2] {
    [
        ("user.reg", Hive::CurrentUser),
        ("system.reg", Hive::LocalMachine),
    ]
}

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

/// Save the initial hives alongside the stopped base filesystem before publication.
pub(super) async fn capture(prefix: &Path, before: &Path) -> Result<()> {
    async_fs::create_dir_all(before).await?;
    for (file, _) in registry_files() {
        async_fs::copy(prefix.join(file), before.join(file)).await?;
    }
    Ok(())
}

/// Write both forward patches after Wine has stopped, including empty changes.
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

/// Committed artifact layers carry registry patches separately from filesystem effects.
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

/// The workspace is stopped and checkpointed. Only managed registry files are replaced;
/// all other private files and whiteouts keep normal overlay precedence. Apply patches
/// to the initial hives in the supplied order, then replay private registry changes.
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
