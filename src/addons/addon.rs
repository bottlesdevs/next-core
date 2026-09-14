//! Immutable addon definitions, frozen recipes, and their family discriminators.

use std::{
    fmt,
    path::{Path, PathBuf},
    str::FromStr,
};

use serde::{Deserialize, Serialize};
use strum::EnumIter;
use uuid::Uuid;

use crate::{
    Directories, EnvVars,
    error::Result,
    runner::{Proton, Runner, RunnerError, RunnerKind, Wine, detect_runner_kind},
};

use super::{
    AddonError,
    catalog::AddonFamily,
    recipe::{InstallResource, InstallStep},
};

/// An immutable addon definition with its complete installation recipe.
///
/// `K` is [`Component`] or [`Dependency`]. The same record is stored alongside its
/// shared payload and embedded in bottle or standalone program state. Recipes are
/// resolved during acquisition and never reconstructed from the catalog on load.
/// A changed definition must have a new UUID; payload availability is separate.
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
    /// Returns the release identifier shared by its catalog, release, and bottle records.
    pub fn id(&self) -> Uuid {
        self.id
    }

    /// Returns the release label.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the release version string.
    pub fn version(&self) -> &str {
        &self.version
    }

    /// Returns the addons that must coexist with this selection.
    pub fn requirements(&self) -> &[Requirement] {
        &self.requirements
    }

    /// Derives launch variables from the frozen recipe in resource and step order.
    ///
    /// Later declarations win. Command-local variables are excluded. Environment
    /// configuration combines addon contributions and applies owner overrides last.
    pub fn env_vars(&self) -> EnvVars {
        let mut env_vars = EnvVars::default();
        for step in self.recipe() {
            if let InstallStep::SetEnvironment { name, value } = step {
                env_vars.insert(name.clone(), value.clone());
            }
        }
        env_vars
    }

    pub(crate) fn path(&self, directories: &Directories) -> PathBuf
    where
        K: AddonFamily,
    {
        self.directory(directories).join("payload")
    }
    pub(super) fn directory(&self, directories: &Directories) -> PathBuf
    where
        K: AddonFamily,
    {
        K::releases(directories).join(self.id().to_string())
    }
    pub(crate) fn resources(&self) -> &[InstallResource] {
        &self.resources
    }
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

    /// Validates the resource structure and component layout at a staged or published payload.
    pub(crate) async fn validate(&self, payload: &Path) -> Result<()> {
        if self.resources.len() != 1 || !self.resources[0].path.as_os_str().is_empty() {
            return Err(AddonError::InvalidRelease(payload.to_path_buf()).into());
        }
        if !async_fs::metadata(payload).await.is_ok_and(|m| m.is_dir()) {
            return Err(AddonError::PayloadMissing(self.id()).into());
        }
        let marker = match self.slot() {
            Slot::Runner => {
                crate::runner::detect_runner_kind(payload).await?;
                None
            }
            Slot::WineBridge => Some("bottles-winebridge.exe"),
            Slot::Umu => Some("umu-run"),
            _ => None,
        };
        let sources = marker
            .map(Path::new)
            .into_iter()
            .chain(self.recipe().filter_map(|step| match step {
                InstallStep::Copy { source, .. } => Some(source.as_path()),
                _ => None,
            }));
        for source in sources {
            if !async_fs::metadata(payload.join(source))
                .await
                .is_ok_and(|entry| entry.is_file())
            {
                return Err(AddonError::InvalidComponent(payload.to_path_buf()).into());
            }
        }
        Ok(())
    }

    /// Returns the mutually exclusive role occupied by this component.
    pub fn slot(&self) -> Slot {
        self.kind.slot
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

    /// Validates unique resource paths and files at a staged or published payload.
    pub(crate) async fn validate(&self, payload: &Path) -> Result<()> {
        let mut names = std::collections::HashSet::new();
        if self.resources.is_empty() || self.resources.iter().any(|r| !names.insert(&r.path)) {
            return Err(AddonError::InvalidRelease(payload.to_path_buf()).into());
        }
        for resource in &self.resources {
            if !async_fs::metadata(payload.join(&resource.path))
                .await
                .is_ok_and(|m| m.is_file())
            {
                return Err(AddonError::PayloadMissing(self.id()).into());
            }
        }
        Ok(())
    }

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

impl<K: Serialize + serde::de::DeserializeOwned + 'static> next_config::Config for Addon<K> {
    const VERSION: u32 = 1;
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

/// Type discriminator for component catalog, release, and bottle records.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Component {
    pub(crate) slot: Slot,
}

/// Type discriminator for dependency catalog, release, and bottle records.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Dependency {}
