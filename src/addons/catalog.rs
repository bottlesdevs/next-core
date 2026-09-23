//! Remote component and dependency catalogs and their validation rules.

use futures_lite::io::AsyncReadExt;
use sha2::{Digest, Sha256, Sha512};
use std::{
    io,
    path::{Path, PathBuf},
    sync::Arc,
};

use serde::{Deserialize, Deserializer, Serialize, de, de::DeserializeOwned};
use url::Url;
use uuid::{NonNilUuid, Uuid};

use crate::{Directories, error::Result};

use super::recipe::InstallStep;
use super::{Component, Dependency, Requirement, Slot};

const CATALOG_VERSION: u32 = 1;

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "algorithm", content = "value", rename_all = "kebab-case")]
/// Expected digest used to verify a downloaded catalog artifact.
///
/// Values are stored without validating their length or encoding. Verification
/// compares them exactly and case-sensitively with a lowercase hexadecimal digest.
pub(crate) enum Checksum {
    /// Uses the `sha256` wire discriminator.
    Sha256(String),
    /// Uses the `sha512` wire discriminator.
    Sha512(String),
}

impl Checksum {
    pub(crate) async fn verify(&self, path: &Path) -> io::Result<bool> {
        let mut file = async_fs::File::open(path).await?;
        let mut buffer = [0; 64 * 1024];
        let mut sha256 = Sha256::new();
        let mut sha512 = Sha512::new();
        loop {
            let read = file.read(&mut buffer).await?;
            if read == 0 {
                break;
            }
            match self {
                Checksum::Sha256(_) => sha256.update(&buffer[..read]),
                Checksum::Sha512(_) => sha512.update(&buffer[..read]),
            }
        }
        let actual = match self {
            Checksum::Sha256(_) => format!("{:x}", sha256.finalize()),
            Checksum::Sha512(_) => format!("{:x}", sha512.finalize()),
        };
        Ok(actual == self.value())
    }

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
/// A missing cache is optional; read and parse failures are returned.
pub(crate) struct Catalog<K> {
    #[serde(deserialize_with = "deserialize_catalog_version")]
    schema_version: u32,
    entries: Vec<CatalogEntry<K>>,
}

impl<K> Catalog<K> {
    /// Loads the cached catalog, returning None only when the cache is absent.
    pub(crate) async fn load(directories: &Directories) -> Result<Option<Arc<Self>>>
    where
        K: AddonFamily,
        Self: DeserializeOwned,
    {
        let bytes = match async_fs::read(K::catalog(directories)).await {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        Ok(Some(Arc::new(serde_json::from_slice(&bytes)?)))
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
/// Keeping this mapping on the two runtime families lets catalog
/// persistence share generic code without introducing per-slot component types.
pub(crate) trait AddonFamily {
    const LABEL: &'static str;

    fn url(urls: &CatalogUrls) -> Option<Url>;
    fn catalog(directories: &Directories) -> PathBuf;
    fn releases(directories: &Directories) -> PathBuf;
}

impl AddonFamily for Component {
    const LABEL: &'static str = "components";

    fn url(urls: &CatalogUrls) -> Option<Url> {
        urls.components.clone()
    }

    fn catalog(directories: &Directories) -> PathBuf {
        directories.components().join("catalog.json")
    }

    fn releases(directories: &Directories) -> PathBuf {
        directories.component_releases()
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

    fn releases(directories: &Directories) -> PathBuf {
        directories.dependency_releases()
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
    name: String,
    version: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    requirements: Vec<Requirement>,
    artifacts: Vec<CatalogArtifact>,
    #[serde(flatten)]
    kind: K,
}

impl<K> CatalogEntry<K> {
    /// Returns the identifier used to correlate this release with a local release.
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

    /// Requirements owned by this release definition, for either addon family.
    pub fn requirements(&self) -> &[Requirement] {
        &self.requirements
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

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
/// One downloadable file and the recipe associated with it.
///
/// Both component and dependency recipes become part of the immutable release.
/// Components are extracted before their recipe is applied.
pub(crate) struct CatalogArtifact {
    url: url::Url,
    file_name: String,
    checksum: Checksum,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    platform: Option<Target>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    steps: Option<Vec<InstallStep>>,
}

impl CatalogArtifact {
    pub(crate) fn url(&self) -> &Url {
        &self.url
    }
    pub(crate) fn file_name(&self) -> &str {
        &self.file_name
    }
    pub(crate) fn checksum(&self) -> &Checksum {
        &self.checksum
    }
    pub(crate) fn steps(&self) -> Option<&[InstallStep]> {
        self.steps.as_deref()
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
