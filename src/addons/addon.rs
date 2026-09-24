//! Installed addon records and the constraints between them.
//!
//! An [`Addon`] is the durable form of a release: it combines catalog metadata
//! with the installation recipe resolved when the release was acquired. The
//! [`Component`] and [`Dependency`] marker types keep the two addon families
//! distinct at compile time.

use std::{fmt, path::PathBuf, str::FromStr};

use serde::{Deserialize, Serialize};
use strum::EnumIter;
use uuid::Uuid;

use crate::{
    Directories, EnvVars,
    error::Result,
    runner::{Proton, Runner, RunnerError, RunnerKind, Wine, detect_runner_kind},
};

use super::{
    AddonFamily,
    recipe::{InstallResource, InstallStep},
};

/// Describes an acquired addon and its frozen installation recipe.
///
/// `K` identifies the release as a [`Component`] or [`Dependency`]. Records are
/// stored next to their shared payload and copied into environment state when the
/// addon is selected. Because the recipe is frozen at acquisition time, later
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
    resources: Vec<InstallResource>,
    #[serde(flatten)]
    kind: K,
}

impl<K> Addon<K> {
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

    /// Collects launch environment variables declared by the frozen recipe.
    ///
    /// Recipe resources and steps are visited in declaration order, so a later
    /// declaration with the same name replaces an earlier one. Variables scoped
    /// to installer commands are not included.
    pub fn env_vars(&self) -> EnvVars {
        let mut env_vars = EnvVars::default();
        self.extend_env_vars(&mut env_vars);
        env_vars
    }

    /// Applies this addon's runtime environment declarations to `env_vars`.
    pub(crate) fn extend_env_vars(&self, env_vars: &mut EnvVars) {
        for step in self.recipe() {
            if let InstallStep::SetEnvironment { name, value } = step {
                env_vars.insert(name.clone(), value.clone());
            }
        }
    }

    /// Returns the shared payload directory for this release.
    pub(crate) fn path(&self, directories: &Directories) -> PathBuf
    where
        K: AddonFamily,
    {
        self.directory(directories).join("payload")
    }
    /// Returns the managed directory containing this release's manifest and payload.
    pub(super) fn directory(&self, directories: &Directories) -> PathBuf
    where
        K: AddonFamily,
    {
        K::releases(directories).join(self.id().to_string())
    }
    /// Returns the frozen installation resources in execution order.
    pub(crate) fn resources(&self) -> &[InstallResource] {
        &self.resources
    }
    /// Iterates over all frozen recipe steps in execution order.
    pub(crate) fn recipe(&self) -> impl DoubleEndedIterator<Item = &InstallStep> {
        self.resources.iter().flat_map(|r| &r.steps)
    }
}

impl Addon<Component> {
    pub(crate) fn new_component(
        id: Uuid,
        name: String,
        version: String,
        slot: Slot,
        requirements: Vec<Requirement>,
        resource: InstallResource,
    ) -> Self {
        Self {
            id,
            name,
            version,
            requirements,
            kind: Component { slot },
            resources: vec![resource],
        }
    }

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

    /// Constructs the runner represented by this component's payload.
    ///
    /// Proton runners require an acquired UMU component supplied through `umu`.
    ///
    /// # Errors
    ///
    /// Returns an error if the runner kind cannot be detected or Proton is selected
    /// without an UMU component.
    pub(crate) async fn load_runner(
        &self,
        directories: &Directories,
        umu: Option<&Self>,
    ) -> Result<Box<dyn Runner>> {
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
    pub(crate) fn new_dependency(
        id: Uuid,
        name: String,
        version: String,
        requirements: Vec<Requirement>,
        resources: Vec<InstallResource>,
    ) -> Self {
        Self {
            id,
            name,
            version,
            requirements,
            kind: Dependency::default(),
            resources,
        }
    }

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

impl<K: Serialize + serde::de::DeserializeOwned + 'static> next_config::Config for Addon<K> {
    const VERSION: u32 = 1;
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
/// assert_eq!("runner".parse::<Slot>()?, Slot::Runner);
/// assert_eq!(Slot::Runner.to_string(), "runner");
/// # Ok::<(), String>(())
/// ```
#[derive(Clone, Copy, Debug, Deserialize, EnumIter, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Slot {
    /// The Bottles `WineBridge` service executable.
    #[serde(rename = "winebridge")]
    WineBridge,
    /// The Wine or Proton runtime used to launch Windows programs.
    Runner,
    /// The `umu-run` compatibility launcher required by Proton runners.
    Umu,
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
            Self::WineBridge => "winebridge",
            Self::Runner => "runner",
            Self::Umu => "umu",
            Self::Dxvk => "dxvk",
            Self::Vkd3d => "vkd3d",
            Self::Nvapi => "nvapi",
            Self::LatencyFlex => "latency-flex",
        }
    }

    /// Returns whether this slot supplies runtime tooling rather than prefix changes.
    pub(crate) fn is_runtime(self) -> bool {
        matches!(self, Self::WineBridge | Self::Runner | Self::Umu)
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
            "winebridge" => Self::WineBridge,
            "runner" => Self::Runner,
            "umu" => Self::Umu,
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
/// Name and identifier constraints may be satisfied by either addon family;
/// slot constraints may be satisfied only by a [`Component`]. Use
/// [`Addon::<Component>::satisfies`] or [`Addon::<Dependency>::satisfies`] to
/// test a candidate.
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
