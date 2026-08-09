//! Discovering, downloading, and describing runners and bottle addons.
//!
//! Obtain the shared [`Addons`] manager from [`crate::Bottles::addons`]. The
//! manager combines Bottles-maintained catalogs with components found in the
//! application data directory. Returned [`Addon`] and [`RunnerComponent`]
//! values are snapshots: fetch an [`Availability::Downloadable`] item, then
//! query the manager again before using it.
//!
//! Downloading an addon only places its resources in library-managed storage.
//! [`crate::Bottle::install`] is the separate operation that applies those
//! resources to a bottle.

use serde::{Deserialize, Deserializer, Serialize, de};

pub(crate) mod catalog;
mod index;
pub mod installer;
pub(crate) mod item;
mod manager;

pub use item::{Addon, Availability, RunnerComponent, Slot};
pub use manager::{AddonError, Addons, CatalogKind};

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "algorithm", content = "value", rename_all = "kebab-case")]
/// Expected digest used to verify a downloaded catalog artifact.
///
/// Values are stored without validating their length or encoding. Catalog
/// deserialization rejects an empty value but does not validate hexadecimal
/// syntax. Verification compares the value exactly and case-sensitively with a
/// lowercase hexadecimal digest.
pub enum Checksum {
    /// Uses the `sha256` wire discriminator.
    Sha256(String),
    /// Uses the `sha512` wire discriminator.
    Sha512(String),
}

impl Checksum {
    /// Stores a SHA-256 expectation verbatim; no format validation is performed.
    pub fn sha256(value: impl Into<String>) -> Self {
        Self::Sha256(value.into())
    }

    /// Stores a SHA-512 expectation verbatim; no format validation is performed.
    pub fn sha512(value: impl Into<String>) -> Self {
        Self::Sha512(value.into())
    }

    /// Exposes the unnormalized string used for exact checksum verification.
    pub fn value(&self) -> &str {
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
pub struct Target {
    os: OperatingSystem,
    arch: Architecture,
}

impl Target {
    pub fn new(os: OperatingSystem, arch: Architecture) -> Self {
        Self { os, arch }
    }

    pub fn linux_x86_64() -> Self {
        Self::new(OperatingSystem::Linux, Architecture::X86_64)
    }

    /// Maps the compile target into the subset represented by this type.
    ///
    /// An unrepresented operating system or architecture yields `None`. Catalog
    /// selection then treats even unrestricted artifacts as unsupported.
    pub fn current() -> Option<Self> {
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

    pub fn os(self) -> OperatingSystem {
        self.os
    }

    pub fn arch(self) -> Architecture {
        self.arch
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
/// Operating systems supported by exact catalog-target matching.
pub enum OperatingSystem {
    Linux,
    MacOs,
    Windows,
}

#[derive(Debug, Clone, Copy, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
/// Architectures supported by exact catalog-target matching.
pub enum Architecture {
    X86,
    #[serde(rename = "x86_64")]
    X86_64,
    Aarch64,
}

/// Rejects empty or whitespace-only input without trimming accepted values.
pub(crate) fn deserialize_non_empty_string<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    let value = String::deserialize(deserializer)?;

    if value.trim().is_empty() {
        return Err(de::Error::custom("value cannot be empty"));
    }

    Ok(value)
}

pub(crate) fn deserialize_non_empty_vec<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
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
