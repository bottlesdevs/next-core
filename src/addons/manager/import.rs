//! Explicit local component archive import. Templates are frozen into fresh releases.

use super::super::{
    AddonError, Component, Release, Requirement, Slot,
    installer::{InstallResource, InstallStep, recipe_steps},
};
use super::{Addons, prepare_component_archive};
use crate::{
    Operation, Progress, Stage,
    error::Result,
    runner::{RunnerKind, detect_runner_kind},
};
use std::{path::Path, sync::Arc};
use uuid::{NonNilUuid, Uuid};

impl Addons {
    /// Imports a local tar, tar.gz/tgz, or tar.xz/txz.Assigns a fresh UUID and freezes the bundled recipe. The source archive is unchanged; directories are not supported.
    pub fn import_component(
        &self,
        path: impl AsRef<Path>,
        slot: Slot,
        name: impl Into<String>,
        version: impl Into<String>,
    ) -> Operation<Arc<Release<Component>>> {
        let source = path.as_ref().to_path_buf();
        let name = name.into();
        let version = version.into();
        let addons = self.clone();
        Operation::new(move |progress, cancellation| async move {
            progress.send_replace(Some(Progress::new(Stage::Preparing)));
            let stage = addons.create_stage().await?;
            let result = async {
                let prepared = prepare_component_archive(&source, &stage, &cancellation).await?;
                let payload = prepared.join("payload");
                let requirements = inspect_release(slot, &payload).await?;
                let steps = recipe_steps(slot).to_vec();
                // Template layouts must match the imported files.
                for step in &steps {
                    if let InstallStep::Copy { source, .. } = step {
                        if !async_fs::metadata(payload.join(source))
                            .await
                            .is_ok_and(|m| m.is_file())
                        {
                            return Err(AddonError::InvalidComponent(payload).into());
                        }
                    }
                }
                let id = Uuid::new_v4();
                let release = Release::new_component(
                    NonNilUuid::new(id).unwrap(),
                    name,
                    version,
                    slot,
                    requirements,
                    InstallResource::new("", steps),
                );
                addons
                    .commit_component(Arc::new(release), &prepared, &cancellation)
                    .await
            }
            .await;
            let _ = async_fs::remove_dir_all(stage).await;
            result
        })
    }
}

async fn inspect_release(slot: Slot, path: &Path) -> Result<Vec<Requirement>> {
    Ok(match slot {
        Slot::Runner if detect_runner_kind(path).await? == RunnerKind::Proton => {
            vec![Requirement::Slot(Slot::Umu)]
        }
        Slot::Nvapi => vec![Requirement::Slot(Slot::Dxvk)],
        _ => Vec::new(),
    })
}
