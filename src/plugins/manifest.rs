use std::{borrow::Cow, convert::Infallible, fmt, path::Path, str::FromStr};

use semver::Version;
use serde::{Deserialize, Serialize};

use super::{MANIFEST_FILE, PluginError, Result};

/// Stable, human-readable identity of a plugin.
#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct PluginId(Cow<'static, str>);

impl PluginId {
    pub const fn new(value: &'static str) -> Self {
        Self(Cow::Borrowed(value))
    }

    pub fn as_str(&self) -> &str {
        self.0.as_ref()
    }
}

impl fmt::Display for PluginId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl FromStr for PluginId {
    type Err = Infallible;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        Ok(Self::from(value.to_owned()))
    }
}

impl From<String> for PluginId {
    fn from(value: String) -> Self {
        Self(Cow::Owned(value))
    }
}

/// Identity and display metadata shipped beside a plugin component.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PluginManifest {
    pub schema_version: u32,
    pub id: PluginId,
    pub name: String,
    pub version: Version,
    pub description: String,
    pub authors: Vec<String>,
    pub license: String,
    pub repository: url::Url,
}

impl PluginManifest {
    pub(super) async fn load(directory: &Path) -> Result<Self> {
        let source = async_fs::read_to_string(directory.join(MANIFEST_FILE)).await?;
        Self::parse(&source)
    }

    fn parse(source: &str) -> Result<Self> {
        let manifest: Self = toml::from_str(source)?;
        if manifest.schema_version != 1 {
            return Err(PluginError::UnsupportedSchema(manifest.schema_version));
        }
        Ok(manifest)
    }
}
