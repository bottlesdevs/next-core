//! Cached remote catalogs and platform-specific release artifacts.
//!
//! Catalogs advertise releases without installing them. A [`CatalogEntry`]
//! becomes an [`Addon`](super::Addon) only after the manager downloads and
//! verifies the artifact selected for the current [`Target`].
//!
//! Artifact file names and recipe paths are not containment-checked before they
//! are joined to managed roots. Catalog documents must therefore come from a
//! trusted source.

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
/// Stores the expected digest for a downloadable artifact.
///
/// Digest strings are accepted as catalog data without normalization. Verification
/// therefore requires an exact, case-sensitive match with the lowercase hexadecimal
/// digest produced for the downloaded file.
pub(crate) enum Checksum {
    /// Verifies the artifact with SHA-256.
    Sha256(String),
    /// Verifies the artifact with SHA-512.
    Sha512(String),
}

impl Checksum {
    /// Computes the selected digest for `path` and compares it with the catalog value.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be opened or read.
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

    /// Returns the catalog value used for exact digest comparison.
    pub(crate) fn value(&self) -> &str {
        match self {
            Self::Sha256(value) | Self::Sha512(value) => value,
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
/// Identifies an operating-system and architecture pair used by catalog artifacts.
///
/// Matching is exact; no compatibility is inferred between platform variants.
pub(crate) struct Target {
    os: OperatingSystem,
    arch: Architecture,
}

impl Target {
    const fn new(os: OperatingSystem, arch: Architecture) -> Self {
        Self { os, arch }
    }

    /// Returns the current build target when its OS and architecture are supported.
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
/// Contains the cached entries for one addon family.
///
/// Deserialization accepts only [`CATALOG_VERSION`], preventing a cache written
/// with an incompatible schema from being used.
pub(crate) struct Catalog<K> {
    #[serde(deserialize_with = "deserialize_catalog_version")]
    schema_version: u32,
    entries: Vec<CatalogEntry<K>>,
}

impl<K> Catalog<K> {
    /// Loads the cached catalog for `K`.
    ///
    /// Returns `None` only when the catalog file does not exist.
    ///
    /// # Errors
    ///
    /// Returns an error if the cache cannot be read or is not a valid catalog for
    /// the current schema.
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

    /// Serializes this catalog over the cache for `K`.
    ///
    /// # Errors
    ///
    /// Returns an error if serialization or writing the cache fails.
    pub(crate) async fn save(&self, directories: &Directories) -> Result<()>
    where
        K: AddonFamily,
        Self: Serialize,
    {
        async_fs::write(K::catalog(directories), serde_json::to_vec(self)?).await?;
        Ok(())
    }

    /// Returns entries in their catalog-defined order.
    pub(crate) fn entries(&self) -> &[CatalogEntry<K>] {
        &self.entries
    }

    /// Finds the entry with UUID `id`.
    pub(crate) fn entry(&self, id: Uuid) -> Option<&CatalogEntry<K>> {
        self.entries.iter().find(|entry| entry.id() == id)
    }
}

/// Holds the optional remote endpoint for each addon family.
pub(crate) struct CatalogUrls {
    /// Component catalog endpoint.
    pub(crate) components: Option<Url>,
    /// Dependency catalog endpoint.
    pub(crate) dependencies: Option<Url>,
}

/// Maps an addon family to its endpoint, catalog cache, and release directory.
///
/// This trait is implemented only by [`Component`] and [`Dependency`].
pub(crate) trait AddonFamily {
    /// Human-readable family name used in progress and errors.
    const LABEL: &'static str;

    /// Selects this family's configured remote endpoint.
    fn url(urls: &CatalogUrls) -> Option<Url>;
    /// Returns this family's catalog cache path.
    fn catalog(directories: &Directories) -> PathBuf;
    /// Returns this family's managed release directory.
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

/// Describes a release advertised by a remote addon catalog.
///
/// `K` identifies the release as a [`Component`] or [`Dependency`]. An entry is
/// only metadata: use [`Addons::fetch_component`](super::Addons::fetch_component)
/// or [`Addons::fetch_dependency`](super::Addons::fetch_dependency) to acquire
/// its payload. Check [`is_supported`](Self::is_supported) before offering it for
/// the current platform.
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
    /// Returns the non-nil identifier shared with the acquired release.
    pub fn id(&self) -> Uuid {
        self.id.get()
    }

    /// Returns the human-readable release name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the catalog-provided version string.
    pub fn version(&self) -> &str {
        &self.version
    }

    /// Returns the constraints that an environment must satisfy for this release.
    pub fn requirements(&self) -> &[Requirement] {
        &self.requirements
    }

    /// Returns whether at least one artifact matches the current build target.
    ///
    /// Platform matching is exact. An artifact without a platform restriction
    /// matches any target represented by this crate. If the current OS or
    /// architecture is not represented, every entry is reported as unsupported.
    pub fn is_supported(&self) -> bool {
        Target::current().is_some_and(|target| self.artifacts_for_target(target).next().is_some())
    }

    /// Iterates over artifacts compatible with `target` in catalog order.
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
    /// Returns the environment slot occupied by this component release.
    pub fn slot(&self) -> Slot {
        self.kind.slot
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
/// Describes one downloadable file and its optional installation recipe.
///
/// Matching artifacts are downloaded in catalog order. Their recipes are copied
/// into the acquired [`Addon`](super::Addon), so later catalog changes do not
/// affect the local release.
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
    /// Returns the artifact download URL.
    pub(crate) fn url(&self) -> &Url {
        &self.url
    }
    /// Returns the payload file name used in local release storage.
    pub(crate) fn file_name(&self) -> &str {
        &self.file_name
    }
    /// Returns the digest required for the downloaded file.
    pub(crate) fn checksum(&self) -> &Checksum {
        &self.checksum
    }
    /// Returns the recipe override supplied by the catalog, if any.
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
