//! Installed addon records and the constraints between them.
//!
//! An [`Addon`] is the durable form of a release, freezing its catalog metadata
//! and any installation recipe when the release is acquired.
//! Runtime tools have distinct types and empty installation resources.

use std::{fmt, path::PathBuf, str::FromStr};

use serde::{Deserialize, Serialize};
use strum::EnumIter;
use uuid::Uuid;

use crate::{
    Directories,
    error::Result,
    runner::{Proton, Runner as RunnerBackend, RunnerError, RunnerKind, Wine, detect_runner_kind},
};

use super::{
    StoredAddon,
    recipe::{InstallResource, InstallStep},
};

/// Describes an acquired addon and its frozen metadata.
///
/// `K` identifies a [`Runner`], [`WineBridge`], [`Umu`], [`Component`], or
/// [`Dependency`]. Records are
/// stored next to their shared payload and copied into environment state when the
/// addon is selected. Because metadata is frozen at acquisition time, later
/// catalog refreshes do not alter existing records.
///
/// The UUID returned by [`id`](Self::id) is the record's stable identity. Code
/// that creates serialized records must assign a new UUID whenever the record's
/// definition changes.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    deny_unknown_fields,
    bound(serialize = "K: Serialize", deserialize = "K: Deserialize<'de>")
)]
pub struct Addon<K> {
    id: Uuid,
    name: String,
    version: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    requirements: Vec<Requirement>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    resources: Vec<InstallResource>,
    #[serde(flatten)]
    kind: K,
}

impl<K> Addon<K> {
    pub(crate) fn new(
        id: Uuid,
        name: String,
        version: String,
        requirements: Vec<Requirement>,
        resources: Vec<InstallResource>,
        kind: K,
    ) -> Self {
        Self {
            id,
            name,
            version,
            requirements,
            resources,
            kind,
        }
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

    /// Returns the constraints that an environment must satisfy for this addon.
    pub fn requirements(&self) -> &[Requirement] {
        &self.requirements
    }

    pub(crate) fn resources(&self) -> &[InstallResource] {
        &self.resources
    }

    pub(crate) fn recipe(&self) -> impl DoubleEndedIterator<Item = &InstallStep> {
        self.resources.iter().flat_map(|resource| &resource.steps)
    }

    /// Returns the shared payload directory for this release.
    pub(crate) fn path(&self, directories: &Directories) -> PathBuf
    where
        K: StoredAddon,
    {
        self.directory(directories).join("payload")
    }
    /// Returns the managed directory containing this release's manifest and payload.
    pub(super) fn directory(&self, directories: &Directories) -> PathBuf
    where
        K: StoredAddon,
    {
        K::releases(directories).join(self.id().to_string())
    }
}

impl Addon<Component> {
    /// Returns the mutually exclusive environment role occupied by the component.
    pub fn slot(&self) -> Slot {
        self.kind.slot
    }

    /// Returns whether this component satisfies `requirement`.
    ///
    /// Names and identifiers are compared exactly; a [`Requirement::Slot`]
    /// matches this component's [`slot`](Self::slot).
    pub fn satisfies(&self, requirement: &Requirement) -> bool {
        match requirement {
            Requirement::Name(name) => self.name == *name,
            Requirement::Slot(slot) => self.slot() == *slot,
            Requirement::Id(id) => self.id() == *id,
        }
    }
}

impl Addon<Runner> {
    /// Constructs the runner represented by this release's payload.
    ///
    /// Proton runners require an acquired UMU launcher supplied through `umu`.
    ///
    /// # Errors
    ///
    /// Returns an error if the runner kind cannot be detected or Proton is selected
    /// without an UMU launcher.
    pub(crate) async fn load_runner(
        &self,
        directories: &Directories,
        umu: Option<&Addon<Umu>>,
    ) -> Result<Box<dyn RunnerBackend>> {
        let path = self.path(directories);
        match detect_runner_kind(&path).await? {
            RunnerKind::Wine => Ok(Box::new(Wine::new(path.join("bin/wine")))),
            RunnerKind::Proton => {
                let umu = umu
                    .ok_or(RunnerError::UmuExecutableMissing)?
                    .path(directories)
                    .join("umu-run");
                Ok(Box::new(Proton::new(&path, umu)))
            }
        }
    }
}

impl Addon<Dependency> {
    /// Returns whether this dependency satisfies `requirement`.
    ///
    /// Names and identifiers are compared exactly. A dependency never satisfies
    /// [`Requirement::Slot`], because only components occupy slots.
    pub fn satisfies(&self, requirement: &Requirement) -> bool {
        match requirement {
            Requirement::Name(name) => self.name == *name,
            Requirement::Slot(_) => false,
            Requirement::Id(id) => self.id() == *id,
        }
    }
}

/// Identifies a mutually exclusive component role in an environment.
///
/// Environment state can select at most one [`Component`] for each slot. The
/// string representation is the canonical spelling used by catalogs and storage.
///
/// # Examples
///
/// ```
/// use bottles_core::Slot;
///
/// assert_eq!("dxvk".parse::<Slot>()?, Slot::Dxvk);
/// assert_eq!(Slot::Dxvk.to_string(), "dxvk");
/// # Ok::<(), String>(())
/// ```
#[derive(Clone, Copy, Debug, Deserialize, EnumIter, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
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

impl Slot {
    /// Returns the canonical catalog and storage spelling of the slot.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Dxvk => "dxvk",
            Self::Vkd3d => "vkd3d",
            Self::Nvapi => "nvapi",
            Self::LatencyFlex => "latency-flex",
        }
    }
}

impl fmt::Display for Slot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for Slot {
    type Err = String;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        Ok(match value {
            "dxvk" => Self::Dxvk,
            "vkd3d" => Self::Vkd3d,
            "nvapi" => Self::Nvapi,
            "latency-flex" => Self::LatencyFlex,
            _ => return Err(format!("unknown addon slot {value:?}")),
        })
    }
}

/// Describes another addon that must be present in an environment.
///
/// Name and identifier constraints may be satisfied by any addon kind;
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

/// Marks an acquired Wine or Proton runtime.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Runner {}

/// Marks the acquired Bottles WineBridge service executable.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WineBridge {}

/// Marks an acquired UMU launcher used with Proton.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Umu {}

/// Marks an addon as a component that occupies one [`Slot`].
///
/// Components are mutually exclusive by slot when selected in an environment.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Component {
    /// Environment role occupied by the component.
    pub(crate) slot: Slot,
}

/// Marks an addon as a dependency that may coexist with other dependencies.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Dependency {}
