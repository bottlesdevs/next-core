//! Catalog/index kinds and strongly typed bottle addons.

use std::{fmt, marker::PhantomData, path::PathBuf, str::FromStr};

use serde::{Deserialize, Serialize};
use strum::EnumIter;
use uuid::{NonNilUuid, Uuid};

use crate::{
    Directories,
    bottle::error::BottleError,
    error::Result,
    runner::{Proton, Runner as RuntimeRunner, RunnerError, RunnerKind, Wine, detect_runner_kind},
};

use super::installer::{InstallStep, recipe_steps};

/// A mutually exclusive component role within a bottle.
#[derive(Clone, Copy, Debug, Deserialize, EnumIter, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Slot {
    #[serde(rename = "winebridge")]
    WineBridge,
    Runner,
    Umu,
    Dxvk,
    Vkd3d,
    Nvapi,
    LatencyFlex,
}

impl Slot {
    /// Returns the canonical catalog and filesystem spelling.
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

/// A dependency that must already be selected or installed in a bottle.
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Requirement {
    /// Any addon with this exact, case-sensitive name.
    Name(String),
    /// The component occupying this slot.
    Slot(Slot),
    /// One exact addon release.
    Id(Uuid),
}

/// Runtime category data carried by component catalog and index entries.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Component {
    pub(crate) slot: Slot,
}

/// Runtime category data carried by dependency catalog and index entries.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Dependency {}

mod private {
    pub trait Sealed {}
}

/// A valid typestate for an addon stored in a bottle.
pub trait AddonKind: private::Sealed {}

/// A stored component typestate with one fixed slot.
pub trait ComponentKind: AddonKind + Sized {
    const SLOT: Slot;

    #[doc(hidden)]
    fn from_state(state: &crate::BottleState) -> Option<&Addon<Self>>;
}

macro_rules! component_kinds {
    ($(($kind:ident, $slot:ident, $state:ident => $component:expr, $doc:literal)),+ $(,)?) => {
        $(
            #[doc = $doc]
            #[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
            pub struct $kind;

            impl private::Sealed for $kind {}
            impl AddonKind for $kind {}
            impl ComponentKind for $kind {
                const SLOT: Slot = Slot::$slot;

                fn from_state(state: &crate::BottleState) -> Option<&Addon<Self>> {
                    let $state = state;
                    $component
                }
            }
        )+
    };
}

component_kinds!(
    (
        WineBridge,
        WineBridge,
        state => Some(&state.winebridge),
        "The WineBridge component selected by a bottle."
    ),
    (
        Runner,
        Runner,
        state => Some(&state.runner),
        "The Wine or Proton runner selected by a bottle."
    ),
    (
        Umu,
        Umu,
        state => state.umu.as_ref(),
        "The UMU launcher selected by a bottle."
    ),
    (
        Dxvk,
        Dxvk,
        state => state.dxvk.as_ref(),
        "The DXVK component selected by a bottle."
    ),
    (
        Vkd3d,
        Vkd3d,
        state => state.vkd3d.as_ref(),
        "The VKD3D component selected by a bottle."
    ),
    (
        Nvapi,
        Nvapi,
        state => state.nvapi.as_ref(),
        "The NVAPI component selected by a bottle."
    ),
    (
        LatencyFlex,
        LatencyFlex,
        state => state.latency_flex.as_ref(),
        "The LatencyFlex component selected by a bottle."
    ),
);

impl private::Sealed for Dependency {}
impl AddonKind for Dependency {}

/// One local dependency artifact and its installation recipe.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct Artifact {
    pub(crate) path: PathBuf,
    pub(crate) steps: Vec<InstallStep>,
}

impl Artifact {
    pub(crate) fn new(path: PathBuf, steps: Vec<InstallStep>) -> Self {
        Self { path, steps }
    }
}

/// A complete downloaded or hand-placed addon snapshot.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    deny_unknown_fields,
    bound(serialize = "K: Serialize", deserialize = "K: Deserialize<'de>")
)]
pub struct IndexEntry<K> {
    id: NonNilUuid,
    name: String,
    version: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    requirements: Vec<Requirement>,
    #[serde(flatten)]
    kind: K,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    artifacts: Vec<Artifact>,
}

impl<K> IndexEntry<K> {
    fn new(
        id: NonNilUuid,
        name: String,
        version: String,
        requirements: Vec<Requirement>,
        kind: K,
        artifacts: Vec<Artifact>,
    ) -> Self {
        Self {
            id,
            name,
            version,
            requirements,
            kind,
            artifacts,
        }
    }

    /// Returns the immutable release identifier.
    pub fn id(&self) -> Uuid {
        self.id.get()
    }

    /// Returns the catalog label, or version directory name for hand-placed components.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the downloaded catalog or hand-placed version string.
    pub fn version(&self) -> &str {
        &self.version
    }

    /// Returns the requirements checked before a bottle mutation.
    pub fn requirements(&self) -> &[Requirement] {
        &self.requirements
    }
}

impl IndexEntry<Component> {
    pub(crate) fn new_component(
        id: NonNilUuid,
        name: String,
        version: String,
        slot: Slot,
        requirements: Vec<Requirement>,
    ) -> Self {
        Self::new(
            id,
            name,
            version,
            requirements,
            Component { slot },
            Vec::new(),
        )
    }

    /// Returns the mutually exclusive role occupied by this component.
    pub fn slot(&self) -> Slot {
        self.kind.slot
    }

    pub(crate) fn path(&self, directories: &Directories) -> PathBuf {
        directories
            .components()
            .join(self.slot().as_str())
            .join(self.version())
    }

    pub(crate) fn artifact(&self, directories: &Directories) -> Artifact {
        Artifact::new(self.path(directories), recipe_steps(self.slot()).to_vec())
    }

    /// Reports whether this component satisfies `requirement`.
    pub fn satisfies(&self, requirement: &Requirement) -> bool {
        match requirement {
            Requirement::Name(name) => self.name == *name,
            Requirement::Slot(slot) => self.slot() == *slot,
            Requirement::Id(id) => self.id() == *id,
        }
    }

    pub(crate) async fn load_runner(
        &self,
        directories: &Directories,
        umu: Option<&Self>,
    ) -> Result<Box<dyn RuntimeRunner>> {
        let path = self.path(directories);
        match detect_runner_kind(&path).await? {
            RunnerKind::Wine => Ok(Box::new(Wine::new(path.join("bin/wine")))),
            RunnerKind::Proton => {
                let umu = umu
                    .ok_or(RunnerError::UmuExecutableMissing)?
                    .path(directories)
                    .join("umu-run");
                if !async_fs::metadata(&umu)
                    .await
                    .is_ok_and(|entry| entry.is_file())
                {
                    return Err(RunnerError::RunnerExecutableNotFound(umu).into());
                }
                Ok(Box::new(Proton::new(&path, umu)))
            }
        }
    }
}

impl IndexEntry<Dependency> {
    pub(crate) fn new_dependency(
        id: NonNilUuid,
        name: String,
        version: String,
        requirements: Vec<Requirement>,
        artifacts: Vec<Artifact>,
    ) -> Self {
        Self::new(
            id,
            name,
            version,
            requirements,
            Dependency::default(),
            artifacts,
        )
    }

    pub(crate) fn artifacts(&self) -> &[Artifact] {
        &self.artifacts
    }

    pub(crate) fn path(&self, directories: &Directories) -> PathBuf {
        directories.dependencies().join(self.id().to_string())
    }

    /// Reports whether this dependency satisfies `requirement`.
    pub fn satisfies(&self, requirement: &Requirement) -> bool {
        match requirement {
            Requirement::Name(name) => self.name == *name,
            Requirement::Slot(_) => false,
            Requirement::Id(id) => self.id() == *id,
        }
    }
}

/// An addon selection persisted in a bottle.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Addon<K: AddonKind> {
    id: NonNilUuid,
    name: String,
    version: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    requirements: Vec<Requirement>,
    #[serde(skip)]
    kind: PhantomData<K>,
}

impl<K: AddonKind> Addon<K> {
    /// Returns the immutable release identifier.
    pub fn id(&self) -> Uuid {
        self.id.get()
    }

    /// Returns the catalog label or hand-placed version name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the exact selected version.
    pub fn version(&self) -> &str {
        &self.version
    }

    /// Returns the requirements checked before a bottle mutation.
    pub fn requirements(&self) -> &[Requirement] {
        &self.requirements
    }

    pub(crate) fn statisfies(&self, requirement: &Requirement) -> bool {
        match requirement {
            Requirement::Name(name) => self.name == *name,
            Requirement::Id(id) => self.id() == *id,
            Requirement::Slot(_) => false,
        }
    }
}

impl<K: ComponentKind> TryFrom<&IndexEntry<Component>> for Addon<K> {
    type Error = BottleError;

    fn try_from(entry: &IndexEntry<Component>) -> std::result::Result<Self, Self::Error> {
        if entry.slot() != K::SLOT {
            return Err(BottleError::InvalidComponentSlot {
                component: entry.id(),
                required: K::SLOT,
            });
        }

        Ok(Self {
            id: entry.id,
            name: entry.name.clone(),
            version: entry.version.clone(),
            requirements: entry.requirements.clone(),
            kind: PhantomData,
        })
    }
}

impl From<&IndexEntry<Dependency>> for Addon<Dependency> {
    fn from(entry: &IndexEntry<Dependency>) -> Self {
        Self {
            id: entry.id,
            name: entry.name.clone(),
            version: entry.version.clone(),
            requirements: entry.requirements.clone(),
            kind: PhantomData,
        }
    }
}

impl<K: ComponentKind> Addon<K> {
    pub(crate) fn path(&self, directories: &Directories) -> PathBuf {
        directories
            .components()
            .join(K::SLOT.as_str())
            .join(self.version())
    }

    pub(crate) fn artifact(&self, directories: &Directories) -> Artifact {
        Artifact::new(self.path(directories), recipe_steps(K::SLOT).to_vec())
    }
}

impl Addon<Runner> {
    pub(crate) async fn load_runner(
        &self,
        directories: &Directories,
        umu: Option<&Addon<Umu>>,
    ) -> Result<Box<dyn RuntimeRunner>> {
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
