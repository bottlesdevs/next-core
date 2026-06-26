pub mod component;
pub mod dependency;

use std::num::NonZeroU32;

use serde::{Deserialize, Deserializer, Serialize, de};

pub trait Catalog {
    type Query;
    type Item;

    fn version(&self) -> NonZeroU32;
    fn query(&self) -> Self::Query;
    fn iter(&self) -> impl ExactSizeIterator<Item = &Self::Item> + DoubleEndedIterator;
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "algorithm", content = "value", rename_all = "kebab-case")]
pub enum Checksum {
    Sha256(String),
    Sha512(String),
}

impl Checksum {
    pub fn sha256(value: impl Into<String>) -> Self {
        Self::Sha256(value.into())
    }

    pub fn sha512(value: impl Into<String>) -> Self {
        Self::Sha512(value.into())
    }

    pub fn value(&self) -> &str {
        match self {
            Self::Sha256(value) | Self::Sha512(value) => value,
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Eq, Hash, PartialEq, Serialize)]
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

    pub fn os(self) -> OperatingSystem {
        self.os
    }

    pub fn arch(self) -> Architecture {
        self.arch
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum OperatingSystem {
    Linux,
    MacOs,
    Windows,
}

#[derive(Debug, Clone, Copy, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Architecture {
    X86,
    #[serde(rename = "x86_64")]
    X86_64,
    Aarch64,
}

pub(self) fn deserialize_non_empty_string<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    let value = String::deserialize(deserializer)?;

    if value.is_empty() {
        return Err(de::Error::custom("value cannot be empty"));
    }

    Ok(value)
}

pub(self) fn deserialize_non_empty_vec<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
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

pub(self) fn deserialize_non_empty_checksum<'de, D>(deserializer: D) -> Result<Checksum, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Checksum::deserialize(deserializer)?;

    if value.value().is_empty() {
        return Err(de::Error::custom("checksum cannot be empty"));
    }

    Ok(value)
}
