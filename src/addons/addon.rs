//! Artifact-free addon selections and their family discriminators.

use std::{fmt, path::PathBuf, str::FromStr};

use serde::{Deserialize, Serialize};
use strum::EnumIter;
use uuid::{NonNilUuid, Uuid};

use crate::{
    Directories,
    error::Result,
    runner::{Proton, Runner, RunnerError, RunnerKind, Wine, detect_runner_kind},
};

/// An addon selection persisted in a bottle.
///
/// `K` is [`Component`] or [`Dependency`]. Unlike an [`IndexEntry`](super::IndexEntry),
/// this value contains no download artifacts; it remains sufficient for requirement
/// validation and for locating or removing a selected component.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    deny_unknown_fields,
    bound(serialize = "K: Serialize", deserialize = "K: Deserialize<'de>")
)]
pub struct Addon<K> {
    id: NonNilUuid,
    name: String,
    version: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    requirements: Vec<Requirement>,
    #[serde(flatten)]
    kind: K,
}

impl<K> Addon<K> {
    pub(super) fn new(
        id: NonNilUuid,
        name: String,
        version: String,
        requirements: Vec<Requirement>,
        kind: K,
    ) -> Self {
        Self {
            id,
            name,
            version,
            requirements,
            kind,
        }
    }

    /// Returns the release identifier shared by its catalog, index, and bottle records.
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

    /// Returns the addons that must coexist with this selection.
    pub fn requirements(&self) -> &[Requirement] {
        &self.requirements
    }
}

impl Addon<Component> {
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

    /// Reports whether this component satisfies `requirement`.
    ///
    /// Name and identifier matching is exact. Slot requirements match the
    /// component's slot.
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
    ) -> Result<Box<dyn Runner>> {
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

impl Addon<Dependency> {
    /// Reports whether this dependency satisfies `requirement`.
    ///
    /// Name and identifier matching is exact. Dependencies never satisfy slot
    /// requirements because slots are occupied only by components.
    pub fn satisfies(&self, requirement: &Requirement) -> bool {
        match requirement {
            Requirement::Name(name) => self.name == *name,
            Requirement::Slot(_) => false,
            Requirement::Id(id) => self.id() == *id,
        }
    }
}

/// A mutually exclusive component role within a bottle.
///
/// Bottle state can select at most one component for each slot.
#[allow(missing_docs)]
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

/// A constraint that must be satisfied by another addon in the bottle.
///
/// Name and identifier requirements may be satisfied by either components or
/// dependencies. Slot requirements can be satisfied only by components.
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

/// Type discriminator for component catalog, index, and bottle records.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Component {
    pub(crate) slot: Slot,
}

/// Type discriminator for dependency catalog, index, and bottle records.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Dependency {}
