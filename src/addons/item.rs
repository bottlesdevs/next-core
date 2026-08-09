//! Values describing runners, addons, and internal components.
//!
//! The manager builds owned values by reconciling catalogs with local files.
//! Existing values do not change after a catalog refresh, download, removal, or
//! filesystem change; query [`crate::Addons`] again for updated state. UUIDs
//! identify logical items across queries, while derived equality compares all
//! persisted state. Their Serde representations belong to internal persistence
//! and are not supported interchange formats.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use uuid::{NonNilUuid, Uuid};

use crate::{
    error::Result,
    runner::{Proton, Runner, RunnerError, RunnerKind, Wine, detect_runner_kind},
};

use super::{
    catalog::InternalRole,
    installer::{InstallResource, InstallStep},
};

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
/// A mutually exclusive component role within a bottle.
///
/// Installing an addon in a slot replaces the addon already occupying that slot.
pub enum Slot {
    Dxvk,
    Vkd3d,
    Nvapi,
    LatencyFlex,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Recorded local-file and platform-support state for a catalog item.
///
/// Availability is cached in the [`Addon`] or [`RunnerComponent`] snapshot that
/// produced it. Obtain a new snapshot from [`crate::Addons`] after the manager
/// changes. `Downloaded` only describes the item's own recorded files; it does
/// not guarantee that those files still exist or that separately managed runtime
/// prerequisites are present.
pub enum Availability {
    /// The item's selected artifacts were recorded in local storage.
    ///
    /// Recorded paths are not revalidated and may no longer exist.
    Downloaded,
    /// A matching artifact exists, but transfer and installation may still fail.
    Downloadable,
    /// No matching artifact exists; a recorded item still reports [`Self::Downloaded`].
    Unsupported,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
/// Internal persistence state; only library-produced serialized values are supported.
pub(crate) struct Resource {
    source: PathBuf,
    steps: Vec<InstallStep>,
}

impl Resource {
    pub(crate) fn new(source: PathBuf, steps: Vec<InstallStep>) -> Self {
        Self { source, steps }
    }

    pub(crate) fn install(&self) -> InstallResource {
        InstallResource {
            source: self.source.clone(),
            steps: self.steps.clone(),
        }
    }

    pub(crate) fn source(&self) -> &Path {
        &self.source
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
/// A Wine or Proton runtime available to bottles.
///
/// Values returned by [`crate::Addons::runners`] are owned copies and do not
/// update after downloads, removals, or filesystem changes. After fetching a
/// runner, query the manager again before passing it to
/// [`crate::Bottle::set_runner`]. The UUID is the runner's logical identity;
/// [`PartialEq`] compares the complete value, including availability and paired
/// internal components.
///
/// The serialized representation is internal persistence data, not a stable
/// interchange format. Only values serialized by this library are supported for
/// deserialization.
pub struct RunnerComponent {
    id: NonNilUuid,
    name: String,
    version: String,
    flavour: RunnerKind,
    path: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    umu: Option<InternalComponent>,
    supported: bool,
}

impl RunnerComponent {
    pub(crate) fn new(
        id: NonNilUuid,
        name: String,
        version: String,
        flavour: RunnerKind,
        path: Option<PathBuf>,
        supported: bool,
    ) -> Self {
        Self {
            id,
            name,
            version,
            flavour,
            path,
            umu: None,
            supported,
        }
    }

    #[cfg(test)]
    pub(crate) fn for_test(flavour: RunnerKind, path: impl Into<PathBuf>) -> Result<Self> {
        Ok(Self::new(
            NonNilUuid::new(Uuid::new_v4()).expect("v4 UUID is non-nil"),
            String::from("test runner"),
            String::from("test"),
            flavour,
            Some(crate::utils::absolute_path(path.into())?),
            true,
        ))
    }

    /// Returns the runner's catalog or local-index identity.
    ///
    /// Use this UUID, rather than [`PartialEq`], to compare logical identity
    /// across snapshots.
    pub fn id(&self) -> Uuid {
        self.id.get()
    }
    /// Returns the catalog label, or the indexed version for a hand-placed runner.
    pub fn name(&self) -> &str {
        &self.name
    }
    /// Returns catalog or index metadata without probing the runtime.
    pub fn version(&self) -> &str {
        &self.version
    }
    /// Returns the recorded classification without inspecting installed files.
    pub fn flavour(&self) -> RunnerKind {
        self.flavour
    }
    /// This library-managed path is exposed for inspection and should be treated
    /// as read-only. It is not revalidated and may become stale if files are
    /// removed after this snapshot was created.
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Returns the runner's own-file availability when this snapshot was created.
    ///
    /// [`Availability::Downloaded`] does not validate the runner executable or,
    /// for Proton, guarantee that the separately managed UMU component is present.
    pub fn availability(&self) -> Availability {
        if self.path.is_some() {
            Availability::Downloaded
        } else if self.supported {
            Availability::Downloadable
        } else {
            Availability::Unsupported
        }
    }

    pub(crate) fn installed_path(&self) -> Result<&Path> {
        self.path
            .as_deref()
            .ok_or_else(|| super::AddonError::ItemNotDownloaded(self.id()).into())
    }

    /// Associates the currently selected UMU component with a Proton snapshot.
    ///
    /// Passing a component for a Wine runner has no effect. Pairing only records an
    /// existing component; it does not download or provision UMU.
    pub(crate) fn pair_umu(&mut self, umu: Option<InternalComponent>) {
        self.umu = match self.flavour {
            RunnerKind::Wine => None,
            RunnerKind::Proton => umu,
        };
    }

    pub(crate) fn umu_path(&self) -> Option<&Path> {
        self.umu.as_ref().map(InternalComponent::path)
    }

    /// Loads an executable runner from the recorded component paths.
    ///
    /// The detected layout must match [`Self::flavour`]. Proton additionally
    /// requires a paired UMU component containing a regular `umu-run` file.
    ///
    /// # Errors
    ///
    /// Returns [`super::AddonError::ItemNotDownloaded`] when no runner directory
    /// is recorded, or a [`RunnerError`] when the runner layout or required UMU
    /// executable is missing.
    pub(crate) async fn load(&self) -> Result<Box<dyn Runner>> {
        let path = self.installed_path()?;
        if detect_runner_kind(path).await? != self.flavour {
            return Err(RunnerError::RunnerNotFound(path.to_path_buf()).into());
        }
        match self.flavour {
            RunnerKind::Wine => Ok(Box::new(Wine::new(path.join("bin/wine")))),
            RunnerKind::Proton => {
                let umu = self
                    .umu_path()
                    .ok_or(RunnerError::UmuExecutableMissing)?
                    .join("umu-run");
                if !async_fs::metadata(&umu)
                    .await
                    .is_ok_and(|entry| entry.is_file())
                {
                    return Err(RunnerError::RunnerExecutableNotFound(umu).into());
                }
                Ok(Box::new(Proton::new(path, umu)))
            }
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
/// A versioned component or dependency that augments a bottle's Wine prefix.
///
/// A [`RunnerComponent`] selects the Wine or Proton runtime; an addon changes
/// the environment built around that runtime. Slot-based addons represent
/// replaceable components such as DXVK, VKD3D, DXVK-NVAPI, or LatencyFleX.
/// Addons without a [`Slot`] are dependencies that can coexist with one another.
/// Installing an addon applies its files and configuration to one bottle;
/// installing another addon in the same slot replaces the previous occupant.
///
/// Downloading and installing are separate operations. [`crate::Addons::fetch`]
/// downloads the addon's artifacts into library-managed storage, after which
/// [`crate::Bottle::install`] applies them to a bottle. Existing values do not
/// update when the addon library or filesystem changes; query
/// [`crate::Addons::addons`] again after fetching or removing an item. The UUID
/// is the addon's logical identity; [`PartialEq`] compares the complete value
/// instead.
///
/// The serialized representation is internal persistence data, not a stable
/// interchange format. Only values serialized by this library are supported for
/// deserialization.
pub struct Addon {
    id: NonNilUuid,
    name: String,
    version: String,
    slot: Option<Slot>,
    resources: Vec<Resource>,
    supported: bool,
}

impl Addon {
    pub(crate) fn new(
        id: NonNilUuid,
        name: String,
        version: String,
        slot: Option<Slot>,
        resources: Vec<Resource>,
        supported: bool,
    ) -> Self {
        Self {
            id,
            name,
            version,
            slot,
            resources,
            supported,
        }
    }

    #[cfg(test)]
    pub(crate) fn for_test(slot: Option<Slot>, path: impl Into<PathBuf>) -> Result<Self> {
        Ok(Self::new(
            NonNilUuid::new(Uuid::new_v4()).expect("v4 UUID is non-nil"),
            String::from("test addon"),
            String::from("test"),
            slot,
            vec![Resource::new(
                crate::utils::absolute_path(path.into())?,
                Vec::new(),
            )],
            true,
        ))
    }

    /// Returns the addon's catalog or local-index identity.
    ///
    /// Use this UUID, rather than [`PartialEq`], to compare logical identity
    /// across snapshots.
    pub fn id(&self) -> Uuid {
        self.id.get()
    }
    /// Returns the catalog label, or the indexed version for a hand-placed addon.
    pub fn name(&self) -> &str {
        &self.name
    }
    /// Returns catalog or index metadata without inspecting artifact contents.
    pub fn version(&self) -> &str {
        &self.version
    }
    /// Returns the replacement boundary; `None` means installations may coexist.
    pub fn slot(&self) -> Option<Slot> {
        self.slot
    }

    /// Returns the addon's own-file availability when this snapshot was created.
    ///
    /// [`Availability::Downloaded`] means every selected artifact was recorded as
    /// present. It does not revalidate the paths or guarantee that installing the
    /// recipe will succeed.
    pub fn availability(&self) -> Availability {
        if !self.resources.is_empty() {
            Availability::Downloaded
        } else if self.supported {
            Availability::Downloadable
        } else {
            Availability::Unsupported
        }
    }

    /// # Errors
    ///
    /// Returns [`super::AddonError::ItemNotDownloaded`] when this snapshot has no
    /// recorded resources.
    pub(crate) fn prepare(&self) -> Result<Vec<InstallResource>> {
        if self.resources.is_empty() {
            return Err(super::AddonError::ItemNotDownloaded(self.id()).into());
        }
        Ok(self.resources.iter().map(Resource::install).collect())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
/// Internal persistence state for a component that is not user-selectable.
///
/// Only library-produced serialized values are supported.
pub(crate) struct InternalComponent {
    id: NonNilUuid,
    role: InternalRole,
    path: PathBuf,
}

impl InternalComponent {
    pub(crate) fn new(id: NonNilUuid, role: InternalRole, path: PathBuf) -> Self {
        Self { id, role, path }
    }

    #[cfg(test)]
    pub(crate) fn for_test(role: InternalRole, path: impl Into<PathBuf>) -> Result<Self> {
        Ok(Self::new(
            NonNilUuid::new(Uuid::new_v4()).expect("v4 UUID is non-nil"),
            role,
            crate::utils::absolute_path(path.into())?,
        ))
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn proton_load_uses_its_paired_umu() {
        futures_lite::future::block_on(async {
            let root = std::env::temp_dir().join(format!("bottles-next-runner-{}", Uuid::new_v4()));
            let proton = root.join("proton");
            let umu = root.join("umu");
            fs::create_dir_all(&proton).unwrap();
            fs::create_dir_all(&umu).unwrap();
            fs::write(proton.join("proton"), []).unwrap();
            fs::write(umu.join("umu-run"), []).unwrap();
            let mut runner = RunnerComponent::new(
                NonNilUuid::new(Uuid::new_v4()).unwrap(),
                "Proton".into(),
                "test".into(),
                RunnerKind::Proton,
                Some(proton),
                true,
            );

            assert!(matches!(
                runner.load().await,
                Err(crate::error::Error::Runner(
                    RunnerError::UmuExecutableMissing
                ))
            ));
            runner.pair_umu(Some(
                InternalComponent::for_test(InternalRole::Umu, umu).unwrap(),
            ));
            runner.load().await.unwrap();
            fs::remove_dir_all(root).unwrap();
        });
    }
}
