use std::{
    collections::HashSet,
    num::{NonZeroU32, NonZeroU64},
};

use serde::{Deserialize, Deserializer, Serialize, de};
use url::Url;
use uuid::Uuid;

use crate::compatibility::{Checksum, deserialize_non_empty_string};

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(
    deny_unknown_fields,
    bound(deserialize = "T: CatalogItem + Deserialize<'de>")
)]
pub struct Catalog<T, const SCHEMA_VERSION: u32> {
    #[serde(
        rename = "schema_version",
        deserialize_with = "deserialize_supported_schema_version::<_, SCHEMA_VERSION>"
    )]
    version: NonZeroU32,

    #[serde(deserialize_with = "deserialize_unique_catalog_items")]
    items: Vec<T>,
}

impl<T, const SCHEMA_VERSION: u32> IntoIterator for Catalog<T, SCHEMA_VERSION> {
    type Item = T;
    type IntoIter = std::vec::IntoIter<T>;

    fn into_iter(self) -> Self::IntoIter {
        self.items.into_iter()
    }
}

impl<'catalog, T, const SCHEMA_VERSION: u32> IntoIterator for &'catalog Catalog<T, SCHEMA_VERSION> {
    type Item = &'catalog T;
    type IntoIter = std::slice::Iter<'catalog, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.items.iter()
    }
}

pub(crate) trait CatalogItem {
    fn uuid(&self) -> Uuid;
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct Artifact {
    pub(crate) url: Url,
    #[serde(deserialize_with = "deserialize_non_empty_string")]
    pub(crate) file_name: String,
    #[serde(deserialize_with = "deserialize_non_empty_checksum")]
    pub(crate) checksum: Checksum,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) size: Option<NonZeroU64>,
}

impl Artifact {
    pub fn url(&self) -> &Url {
        &self.url
    }

    pub fn file_name(&self) -> &str {
        &self.file_name
    }

    pub fn checksum(&self) -> &Checksum {
        &self.checksum
    }

    pub fn size(&self) -> Option<u64> {
        self.size.map(|s| s.get())
    }
}

fn deserialize_supported_schema_version<'de, D, const SUPPORTED_VERSION: u32>(
    deserializer: D,
) -> Result<NonZeroU32, D::Error>
where
    D: Deserializer<'de>,
{
    let schema_version = NonZeroU32::deserialize(deserializer)?;
    let supported_schema_version = NonZeroU32::new(SUPPORTED_VERSION)
        .ok_or_else(|| de::Error::custom("supported schema version cannot be zero"))?;

    if schema_version != supported_schema_version {
        return Err(de::Error::custom(format!(
            "unsupported catalog schema version {schema_version}; expected {supported_schema_version}"
        )));
    }

    Ok(schema_version)
}

fn deserialize_unique_catalog_items<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: CatalogItem + Deserialize<'de>,
{
    let items = Vec::<T>::deserialize(deserializer)?;
    let mut ids = HashSet::new();

    for item in &items {
        if !ids.insert(item.uuid()) {
            return Err(de::Error::custom(format!(
                "duplicate catalog item id {}",
                item.uuid()
            )));
        }
    }

    Ok(items)
}

fn deserialize_non_empty_checksum<'de, D>(deserializer: D) -> Result<Checksum, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Checksum::deserialize(deserializer)?;

    if value.value().is_empty() {
        return Err(de::Error::custom("checksum cannot be empty"));
    }

    Ok(value)
}
