//! Addon kinds, requirements, and selections stored by bottle APIs.

use std::{fmt, path::PathBuf, str::FromStr};

use serde::{Deserialize, Serialize};
use strum::EnumIter;
use uuid::{NonNilUuid, Uuid};

use crate::{
    Directories,
    error::Result,
    runner::{Proton, Runner, RunnerError, RunnerKind, Wine, detect_runner_kind},
};

use super::installer::{InstallStep, recipe_steps};

/// An addon selection persisted in a bottle.
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
    pub fn satisfies(&self, requirement: &Requirement) -> bool {
        match requirement {
            Requirement::Name(name) => self.name == *name,
            Requirement::Slot(_) => false,
            Requirement::Id(id) => self.id() == *id,
        }
    }
}

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

/// Category data carried by a downloaded component.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Component {
    pub(crate) slot: Slot,
}

/// Category data carried by a downloaded dependency.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Dependency {}

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
