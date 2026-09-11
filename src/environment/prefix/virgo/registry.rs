//! Compose the managed registry baseline, then replay private changes over it.

use super::{VirgoError, artifacts::cache};
use crate::{
    Context,
    error::{Error, Result},
};
use fvs_rs::{Layer, UnmountMode};
use regdiff_rs::prelude::{Diff, Hive, Registry, apply_files};
use std::{fs, path::Path};
use uuid::Uuid;

pub(crate) fn registry_files() -> [(&'static str, Hive); 2] {
    [
        ("user.reg", Hive::CurrentUser),
        ("system.reg", Hive::LocalMachine),
    ]
}

pub(crate) fn write_forward(old: &Path, new: &Path, output: &Path, hive: Hive) -> Result<()> {
    let old =
        Registry::try_from(old, hive).map_err(|error| VirgoError::Registry(error.to_string()))?;
    let new =
        Registry::try_from(new, hive).map_err(|error| VirgoError::Registry(error.to_string()))?;
    Registry::diff(&old, &new)
        .serialize_file(output)
        .map_err(|error| VirgoError::Registry(error.to_string()))?;
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

/// The owner is stopped and checkpointed. Only managed registry files are replaced;
/// all other private files and whiteouts keep normal overlay precedence.
pub(crate) async fn compose(
    root: &Path,
    layers: &[Layer],
    addons: &[Uuid],
    cx: &Context,
) -> Result<()> {
    let stage = cx
        .directories()
        .data_dir()
        .join("virgo/.staging")
        .join(Uuid::new_v4().to_string());
    let prefix = stage.join("prefix");
    let baseline = stage.join("baseline");
    async_fs::create_dir_all(&prefix).await?;
    async_fs::create_dir_all(&baseline).await?;
    let client = cx.fvs().await?;
    let mount = client
        .mount(&prefix, layers.to_vec(), None::<&Path>)
        .await?;
    let copied = async {
        for (file, _) in registry_files() {
            async_fs::copy(prefix.join(file), baseline.join(file)).await?;
        }
        Ok::<_, Error>(())
    }
    .await;
    // This scratch mount never contains the owner's upper. Release it before
    // changing owner data; on failure retain the mountpoint for explicit cleanup.
    client
        .unmount(&mount, UnmountMode::Normal)
        .await
        .map_err(|source| crate::EnvironmentError::Cleanup {
            prefix,
            source: Box::new(source.into()),
        })?;
    let result = async {
        copied?;
        for id in addons {
            cache::apply_registry(&baseline, *id, cx).await?;
        }
        let root = root.to_path_buf();
        let stage = stage.clone();
        blocking::unblock(move || {
            let previous = root.join("registry-baseline");
            let upper = root.join("upper");
            let merged = stage.join("merged");
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
        .await
    }
    .await;
    let _ = async_fs::remove_dir_all(stage).await;
    result
}
