//! Installed addon records and the constraints between them.
//!
//! An [`Addon`] is the durable form of a release, freezing its catalog metadata
//! and any installation recipe when the release is acquired.
//! Runtime tools have distinct types and empty installation resources.
//! [`AddonRole`] is the sum of the marker types, and [`RoleMarker`] is the bound
//! satisfied by each marker and by [`AddonRole`] itself.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use strum::EnumIter;
use uuid::Uuid;

use crate::{
    error::Result,
    runner::{Proton, Runner as RunnerBackend, RunnerError, Wine, WineArch},
    utils::paths,
};

use super::{
    AddonSettingsSchema,
    recipe::{InstallResource, InstallStep},
};

/// Describes an acquired addon and its frozen metadata.
///
/// `R` is a [`RoleMarker`]: [`Runner`], [`WineBridge`], [`Umu`], [`Component`],
/// [`Dependency`], or [`AddonRole`] for a release of any role. Records are
/// stored next to their shared payload and copied into prefix state when the
/// addon is selected. Because metadata is frozen at acquisition time, later
/// catalog refreshes do not alter existing records.
///
/// The UUID returned by [`id`](Self::id) is the record's stable identity. Code
/// that creates serialized records must assign a new UUID whenever the record's
/// definition changes.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(bound(serialize = "R: Serialize", deserialize = "R: Deserialize<'de>"))]
pub struct Addon<R> {
    id: Uuid,
    name: String,
    version: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    requirements: Vec<Requirement>,
    #[serde(flatten)]
    settings: AddonSettingsSchema,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    resources: Vec<InstallResource>,
    #[serde(flatten)]
    role: R,
}

impl<R> Addon<R> {
    pub(crate) fn new(
        id: Uuid,
        name: String,
        version: String,
        requirements: Vec<Requirement>,
        resources: Vec<InstallResource>,
        role: R,
    ) -> Self {
        Self {
            id,
            name,
            version,
            requirements,
            settings: AddonSettingsSchema::default(),
            resources,
            role,
        }
    }

    /// Adds the catalog's settings and effects to a new record.
    pub(crate) fn with_settings(mut self, settings: AddonSettingsSchema) -> Self {
        self.settings = settings;
        self
    }

    /// Returns the stable identifier shared by the catalog and acquired record.
    pub fn id(&self) -> Uuid {
        self.id
    }

    /// Returns the human-readable release name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the catalog-provided version string.
    pub fn version(&self) -> &str {
        &self.version
    }

    /// Returns the constraints that a prefix must satisfy for this addon.
    pub fn requirements(&self) -> &[Requirement] {
        &self.requirements
    }

    /// Returns the settings this release declares and the effects they drive.
    pub fn settings(&self) -> &AddonSettingsSchema {
        &self.settings
    }

    /// Returns the role of this release.
    pub fn role(&self) -> AddonRole
    where
        R: RoleMarker,
    {
        self.role.into()
    }

    pub(crate) fn metadata(&self) -> (Uuid, &str, &[Requirement]) {
        (self.id, &self.name, &self.requirements)
    }

    pub(crate) fn resources(&self) -> &[InstallResource] {
        &self.resources
    }

    pub(crate) fn recipe(&self) -> impl DoubleEndedIterator<Item = &InstallStep> {
        self.resources.iter().flat_map(|resource| &resource.steps)
    }

    /// Returns the shared payload directory for this release.
    pub(crate) fn path(&self) -> PathBuf
    where
        R: RoleMarker,
    {
        self.directory().join("payload")
    }

    /// Returns the managed directory containing this release's manifest and payload.
    pub(super) fn directory(&self) -> PathBuf
    where
        R: RoleMarker,
    {
        paths::releases().join(self.id().to_string())
    }

    /// Converts this record to role `J`, or returns `None` if its role differs.
    ///
    /// Casting to [`AddonRole`] always succeeds.
    pub fn cast<J: RoleMarker>(self) -> Option<Addon<J>>
    where
        R: RoleMarker,
    {
        Some(Addon {
            role: J::try_from(self.role.into()).ok()?,
            id: self.id,
            name: self.name,
            version: self.version,
            requirements: self.requirements,
            settings: self.settings,
            resources: self.resources,
        })
    }

    pub(super) fn erase(self) -> Addon<AddonRole>
    where
        R: RoleMarker,
    {
        Addon {
            role: self.role.into(),
            id: self.id,
            name: self.name,
            version: self.version,
            requirements: self.requirements,
            settings: self.settings,
            resources: self.resources,
        }
    }
}

impl Addon<Component> {
    /// Returns the mutually exclusive prefix position occupied by the component.
    pub fn slot(&self) -> Slot {
        self.role.slot
    }
}

impl Addon<Runner> {
    /// Returns how this runner is launched.
    pub fn flavor(&self) -> RunnerFlavor {
        self.role.flavor
    }

    /// Constructs the runner represented by this release's payload.
    ///
    /// Proton runners require an acquired UMU launcher supplied through `umu`
    /// and always use [`WineArch::Win64`]; prefix validation rejects any other
    /// `arch` for them.
    ///
    /// # Errors
    ///
    /// Returns [`RunnerError::UmuExecutableMissing`] if Proton is selected
    /// without an UMU launcher.
    pub(crate) fn runner(
        &self,
        umu: Option<&Addon<Umu>>,
        arch: WineArch,
    ) -> Result<Box<dyn RunnerBackend>> {
        let path = self.path();
        match self.flavor() {
            RunnerFlavor::Wine => Ok(Box::new(Wine::new(path.join("bin/wine"), arch))),
            RunnerFlavor::Proton => {
                let umu = umu
                    .ok_or(RunnerError::UmuExecutableMissing)?
                    .path()
                    .join("umu-run");
                Ok(Box::new(Proton::new(&path, umu)))
            }
        }
    }
}

/// Identifies a mutually exclusive component position in a prefix.
///
/// Prefix state can select at most one [`Component`] for each slot.
#[derive(
    Clone,
    Copy,
    Debug,
    Deserialize,
    EnumIter,
    Eq,
    Hash,
    PartialEq,
    Serialize,
    strum::Display,
    strum::IntoStaticStr,
)]
#[serde(rename_all = "kebab-case")]
#[strum(serialize_all = "kebab-case")]
pub enum Slot {
    /// The DXVK Direct3D 8–11 translation layer.
    Dxvk,
    /// The VKD3D-Proton Direct3D 12 translation layer.
    Vkd3d,
    /// The DXVK-NVAPI implementation used by supported NVIDIA workloads.
    Nvapi,
    /// The `LatencyFleX` Vulkan layer and Wine integration.
    LatencyFlex,
}

/// Describes another addon that must be present in a prefix.
///
/// Name and identifier constraints may be satisfied by an addon of any role;
/// slot constraints may be satisfied only by a [`Component`].
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Requirement {
    /// Requires an addon with this exact, case-sensitive name.
    Name(String),
    /// Requires a component that occupies the given slot.
    Slot(Slot),
    /// Requires the addon with this exact release UUID.
    Id(Uuid),
}

/// Identifies the role of an addon in a prefix.
///
/// Catalog entries and shared storage hold releases of every role, so this is
/// the role checked at runtime. Each variant holds the marker used as the type
/// parameter of [`Addon`], and `Addon<AddonRole>` is a release of any role.
#[derive(
    Clone,
    Copy,
    Debug,
    Deserialize,
    Eq,
    PartialEq,
    Serialize,
    derive_more::From,
    derive_more::TryInto,
    strum::EnumIs,
)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum AddonRole {
    /// A Wine or Proton runner.
    Runner(Runner),
    /// The Bottles `WineBridge` executable.
    #[serde(rename = "winebridge")]
    WineBridge(WineBridge),
    /// The independently selected UMU launcher.
    Umu(Umu),
    /// A replaceable component occupying one prefix slot.
    Component(Component),
    /// An installable dependency.
    Dependency(Dependency),
}

/// Bounds the type parameter of an [`Addon`].
///
/// Satisfied by the marker types [`Runner`], [`WineBridge`], [`Umu`],
/// [`Component`], and [`Dependency`], and by [`AddonRole`], which stands for a
/// release of any role.
pub trait RoleMarker:
    TryFrom<AddonRole> + Into<AddonRole> + Copy + PartialEq + Send + Sync + 'static
{
}

impl<R> RoleMarker for R where
    R: TryFrom<AddonRole> + Into<AddonRole> + Copy + PartialEq + Send + Sync + 'static
{
}

/// Marks an acquired Wine or Proton runtime.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Runner {
    /// How the runner is launched.
    pub flavor: RunnerFlavor,
}

/// Identifies how a [`Runner`] is launched.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum RunnerFlavor {
    /// Invokes the release's `bin/wine` directly.
    Wine,
    /// Launches the release through a paired [`Umu`] launcher.
    Proton,
}

/// Marks the acquired Bottles WineBridge service executable.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WineBridge {}

/// Marks an acquired UMU launcher used with Proton.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Umu {}

/// Marks an addon as a component that occupies one [`Slot`].
///
/// Components are mutually exclusive by slot when selected in a prefix.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Component {
    /// Prefix position occupied by the component.
    pub slot: Slot,
}

/// Marks an addon as a dependency that may coexist with other dependencies.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Dependency {}
