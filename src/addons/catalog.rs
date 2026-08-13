//! Remote component and dependency catalogs and their validation rules.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use serde::{Deserialize, Deserializer, Serialize, de, de::DeserializeOwned};
use url::Url;
use uuid::{NonNilUuid, Uuid};

use crate::{Directories, error::Result};

use super::installer::InstallStep;
use super::{Component, Dependency, Requirement, Slot, deserialize_non_empty_string};

const CATALOG_VERSION: u32 = 1;

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "algorithm", content = "value", rename_all = "kebab-case")]
/// Expected digest used to verify a downloaded catalog artifact.
///
/// Values are stored without validating their length or encoding. Catalog
/// deserialization rejects an empty value but does not validate hexadecimal
/// syntax. Verification compares the value exactly and case-sensitively with a
/// lowercase hexadecimal digest.
pub(crate) enum Checksum {
    /// Uses the `sha256` wire discriminator.
    Sha256(String),
    /// Uses the `sha512` wire discriminator.
    Sha512(String),
}

impl Checksum {
    /// Exposes the unnormalized string used for exact checksum verification.
    pub(crate) fn value(&self) -> &str {
        match self {
            Self::Sha256(value) | Self::Sha512(value) => value,
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
/// Host operating-system and architecture pair used by catalog artifacts.
///
/// Matching is exact; the library does not infer compatibility between OS or
/// architecture variants.
pub(crate) struct Target {
    os: OperatingSystem,
    arch: Architecture,
}

impl Target {
    const fn new(os: OperatingSystem, arch: Architecture) -> Self {
        Self { os, arch }
    }

    /// Maps the compile target into the subset represented by this type.
    pub(crate) fn current() -> Option<Self> {
        let os = if cfg!(target_os = "linux") {
            OperatingSystem::Linux
        } else if cfg!(target_os = "macos") {
            OperatingSystem::MacOs
        } else if cfg!(target_os = "windows") {
            OperatingSystem::Windows
        } else {
            return None;
        };
        let arch = if cfg!(target_arch = "x86") {
            Architecture::X86
        } else if cfg!(target_arch = "x86_64") {
            Architecture::X86_64
        } else if cfg!(target_arch = "aarch64") {
            Architecture::Aarch64
        } else {
            return None;
        };
        Some(Self::new(os, arch))
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum OperatingSystem {
    Linux,
    MacOs,
    Windows,
}

#[derive(Debug, Clone, Copy, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum Architecture {
    X86,
    #[serde(rename = "x86_64")]
    X86_64,
    Aarch64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
/// One validated, cached catalog document.
///
/// Catalog loading is deliberately tolerant: an unavailable or invalid cache
/// is treated as absent so local index entries remain usable.
pub(crate) struct Catalog<K> {
    #[serde(deserialize_with = "deserialize_catalog_version")]
    schema_version: u32,
    entries: Vec<CatalogEntry<K>>,
}

impl<K> Catalog<K> {
    /// Loads the cached catalog when it is both readable and valid.
    ///
    /// Missing, unreadable, and malformed catalogs are treated as absent so
    /// local addons remain usable and a later refresh can replace the cache.
    pub(crate) async fn load(directories: &Directories) -> Option<Arc<Self>>
    where
        K: AddonFamily,
        Self: DeserializeOwned,
    {
        let catalog =
            serde_json::from_slice(&async_fs::read(K::catalog(directories)).await.ok()?).ok()?;
        Some(Arc::new(catalog))
    }

    /// Replaces the cached catalog for this family.
    pub(crate) async fn save(&self, directories: &Directories) -> Result<()>
    where
        K: AddonFamily,
        Self: Serialize,
    {
        async_fs::write(K::catalog(directories), serde_json::to_vec(self)?).await?;
        Ok(())
    }

    pub(crate) fn entries(&self) -> &[CatalogEntry<K>] {
        &self.entries
    }

    pub(crate) fn entry(&self, id: Uuid) -> Option<&CatalogEntry<K>> {
        self.entries.iter().find(|entry| entry.id() == id)
    }
}

/// Optional remote endpoints for the two supported addon families.
pub(crate) struct CatalogUrls {
    pub(crate) components: Option<Url>,
    pub(crate) dependencies: Option<Url>,
}

/// Maps a family discriminator to its catalog URL and managed storage files.
///
/// Keeping this mapping on the two runtime families lets catalog and index
/// persistence share generic code without introducing per-slot component types.
pub(crate) trait AddonFamily {
    const LABEL: &'static str;

    fn url(urls: &CatalogUrls) -> Option<Url>;
    fn catalog(directories: &Directories) -> PathBuf;
    fn index(directories: &Directories) -> PathBuf;
}

impl AddonFamily for Component {
    const LABEL: &'static str = "components";

    fn url(urls: &CatalogUrls) -> Option<Url> {
        urls.components.clone()
    }

    fn catalog(directories: &Directories) -> PathBuf {
        directories.components().join("catalog.json")
    }

    fn index(directories: &Directories) -> PathBuf {
        directories.components().join("index.toml")
    }
}

impl AddonFamily for Dependency {
    const LABEL: &'static str = "dependencies";

    fn url(urls: &CatalogUrls) -> Option<Url> {
        urls.dependencies.clone()
    }

    fn catalog(directories: &Directories) -> PathBuf {
        directories.dependencies().join("catalog.json")
    }

    fn index(directories: &Directories) -> PathBuf {
        directories.dependencies().join("index.toml")
    }
}

/// A release advertised by a remote addon catalog.
///
/// `K` is [`Component`] or [`Dependency`]. A catalog entry describes what can
/// be fetched; it does not imply that the release supports the current platform
/// or is present in shared storage.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogEntry<K> {
    id: NonNilUuid,
    #[serde(deserialize_with = "deserialize_non_empty_string")]
    name: String,
    #[serde(deserialize_with = "deserialize_non_empty_string")]
    version: String,
    // Dependency requirements come from the catalog. Component requirements
    // are derived from the downloaded release during inspection.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    requirements: Vec<Requirement>,
    #[serde(deserialize_with = "deserialize_non_empty_vec")]
    artifacts: Vec<CatalogArtifact>,
    #[serde(flatten)]
    kind: K,
}

impl<K> CatalogEntry<K> {
    /// Returns the identifier used to correlate this release with an index entry.
    pub fn id(&self) -> Uuid {
        self.id.get()
    }

    /// Returns the catalog label.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the catalog version string.
    pub fn version(&self) -> &str {
        &self.version
    }

    /// Reports whether at least one artifact matches the current build target.
    ///
    /// Platform matching is exact. An artifact without a platform restriction
    /// matches every represented target. Builds on an unrepresented operating
    /// system or architecture report every entry as unsupported.
    pub fn is_supported(&self) -> bool {
        Target::current().is_some_and(|target| self.artifacts_for_target(target).next().is_some())
    }

    pub(crate) fn artifacts_for_target(
        &self,
        target: Target,
    ) -> impl Iterator<Item = &CatalogArtifact> {
        self.artifacts
            .iter()
            .filter(move |artifact| artifact.matches(target))
    }
}

impl CatalogEntry<Component> {
    /// Returns the component slot occupied by this release.
    pub fn slot(&self) -> Slot {
        self.kind.slot
    }
}

impl CatalogEntry<Dependency> {
    /// Returns the addons that must already be present before installation.
    pub fn requirements(&self) -> &[Requirement] {
        &self.requirements
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
/// One downloadable file and the recipe associated with it.
///
/// Dependency recipes are retained in the local index. Components are inspected
/// after extraction and use the built-in recipe for their slot instead.
///
/// `component_root` names the subdirectory *of the archive's single top-level
/// directory* that is the component itself, for archives that wrap it in
/// unrelated packaging such as a macOS app bundle. Naming it relative to that
/// directory keeps the selector independent of a versioned top-level name.
pub(crate) struct CatalogArtifact {
    url: url::Url,
    #[serde(deserialize_with = "deserialize_non_empty_string")]
    file_name: String,
    #[serde(deserialize_with = "deserialize_checksum")]
    checksum: Checksum,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    platform: Option<Target>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    component_root: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    steps: Vec<InstallStep>,
}

impl CatalogArtifact {
    pub(crate) fn url(&self) -> &url::Url {
        &self.url
    }

    pub(crate) fn file_name(&self) -> &str {
        &self.file_name
    }

    pub(crate) fn checksum(&self) -> &Checksum {
        &self.checksum
    }

    pub(crate) fn steps(&self) -> &[InstallStep] {
        &self.steps
    }

    pub(crate) fn component_root(&self) -> Option<&Path> {
        self.component_root.as_deref()
    }

    fn matches(&self, target: Target) -> bool {
        self.platform.is_none_or(|platform| platform == target)
    }
}

fn deserialize_catalog_version<'de, D>(deserializer: D) -> std::result::Result<u32, D::Error>
where
    D: Deserializer<'de>,
{
    let version = u32::deserialize(deserializer)?;
    if version != CATALOG_VERSION {
        return Err(de::Error::custom(format!(
            "unsupported catalog schema version {version}; expected {CATALOG_VERSION}"
        )));
    }
    Ok(version)
}

fn deserialize_checksum<'de, D>(deserializer: D) -> std::result::Result<Checksum, D::Error>
where
    D: Deserializer<'de>,
{
    let checksum = Checksum::deserialize(deserializer)?;
    if checksum.value().is_empty() {
        return Err(de::Error::custom("checksum cannot be empty"));
    }
    Ok(checksum)
}

fn deserialize_non_empty_vec<'de, D, T>(deserializer: D) -> std::result::Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    let value = Vec::<T>::deserialize(deserializer)?;
    if value.is_empty() {
        return Err(de::Error::custom("value cannot be empty"));
    }
    Ok(value)
}
