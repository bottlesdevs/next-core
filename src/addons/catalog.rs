//! Cached remote catalogs and platform-specific release artifacts.
//!
//! Catalogs advertise releases without installing them. A [`CatalogEntry`]
//! becomes an [`Addon`](super::Addon) only after the manager downloads and
//! verifies its compatible artifacts for the current [`Target`].
//!
//! Artifact file names and recipe paths are not containment-checked before they
//! are joined to managed roots. Catalog documents must therefore come from a
//! trusted source.

use futures_lite::io::AsyncReadExt;
use sha2::{Digest, Sha256, Sha512};
use std::{io, path::Path, sync::Arc};

use serde::{Deserialize, Deserializer, Serialize, de};
use url::Url;
use uuid::{NonNilUuid, Uuid};

use crate::error::Result;

use super::recipe::InstallStep;
use super::{Requirement, Slot};

const CATALOG_VERSION: u32 = 1;

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "algorithm", content = "value", rename_all = "kebab-case")]
/// Stores the expected digest for a downloadable artifact.
///
/// Digest strings are accepted as catalog data without normalization. Verification
/// therefore requires an exact, case-sensitive match with the lowercase hexadecimal
/// digest produced for the downloaded file.
pub(crate) enum Checksum {
    Sha256(String),
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
/// Contains the cached entries from one catalog source.
///
/// Deserialization accepts only [`CATALOG_VERSION`], preventing a cache written
/// with an incompatible schema from being used.
pub(crate) struct Catalog {
    #[serde(deserialize_with = "deserialize_catalog_version")]
    schema_version: u32,
    entries: Vec<CatalogEntry>,
}

impl Catalog {
    /// Loads the cached catalog at `path`.
    ///
    /// Returns `None` only when the catalog file does not exist.
    ///
    /// # Errors
    ///
    /// Returns an error if the cache cannot be read or is not a valid catalog for
    /// the current schema.
    pub(crate) async fn load(path: &Path) -> Result<Option<Arc<Self>>> {
        let bytes = match async_fs::read(path).await {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        Ok(Some(Arc::new(serde_json::from_slice(&bytes)?)))
    }

    /// Serializes this catalog over the cache at `path`.
    ///
    /// # Errors
    ///
    /// Returns an error if serialization or writing the cache fails.
    pub(crate) async fn save(&self, path: &Path) -> Result<()> {
        async_fs::write(path, serde_json::to_vec(self)?).await?;
        Ok(())
    }

    /// Returns entries in their catalog-defined order.
    pub(crate) fn entries(&self) -> &[CatalogEntry] {
        &self.entries
    }

    pub(crate) fn entry(&self, id: Uuid) -> Option<&CatalogEntry> {
        self.entries.iter().find(|entry| entry.id() == id)
    }
}

/// Identifies the runtime role or prefix contribution advertised by a catalog entry.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum AddonKind {
    /// A Wine or Proton runner.
    Runner,
    /// The Bottles `WineBridge` executable.
    #[serde(rename = "winebridge")]
    WineBridge,
    /// The independently selected UMU launcher.
    Umu,
    /// A replaceable component occupying one prefix slot.
    Component {
        /// Prefix role occupied by the component.
        slot: Slot,
    },
    /// An installable dependency.
    Dependency,
}

/// Describes a release advertised by a remote addon catalog.
///
/// An entry is only metadata. Its [`kind`](Self::kind) selects the corresponding
/// acquisition method on [`Addons`](super::Addons). Check
/// [`is_supported`](Self::is_supported) before offering it for the current platform.
///
/// # Examples
///
/// ```
/// use bottles_core::CatalogEntry;
///
/// fn supported(entries: &[CatalogEntry]) -> Vec<&CatalogEntry> {
///     entries.iter().filter(|entry| entry.is_supported()).collect()
/// }
/// ```
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CatalogEntry {
    id: NonNilUuid,
    name: String,
    version: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    requirements: Vec<Requirement>,
    artifacts: Vec<CatalogArtifact>,
    #[serde(flatten)]
    kind: AddonKind,
}

impl CatalogEntry {
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

    /// Returns the runtime role or prefix contribution of this release.
    pub fn kind(&self) -> AddonKind {
        self.kind
    }

    /// Returns the constraints that an environment must satisfy for this release.
    pub fn requirements(&self) -> &[Requirement] {
        &self.requirements
    }

    /// Returns whether at least one artifact matches the current build target.
    ///
    /// Platform matching is exact. An artifact without a platform restriction
    /// matches every supported target. Supported operating systems are Linux,
    /// macOS, and Windows; supported architectures are `x86`, `x86_64`, and
    /// `aarch64`.
    /// On other build targets, every entry is reported as unsupported. For
    /// archive releases, multiple matching artifacts make this method return
    /// `true`, but acquisition rejects the ambiguous entry.
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

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
/// Describes one downloadable file and its optional installation recipe.
///
/// Components and dependencies declare installation `steps`, including launch
/// variables, that are frozen into the acquired [`Addon`](super::Addon). When
/// steps are absent, components substitute the slot's default recipe and
/// dependencies use an empty recipe. Later catalog changes do not affect the
/// local release.
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
