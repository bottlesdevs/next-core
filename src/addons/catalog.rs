use std::collections::HashSet;
use std::path::{Component, Path};

use serde::{Deserialize, Deserializer, Serialize, de};
use uuid::{NonNilUuid, Uuid};

use crate::{
    addons::{
        Architecture, Checksum, Target, deserialize_non_empty_string, deserialize_non_empty_vec,
    },
    runner::RunnerKind,
};

use super::installer::InstallStep;

const CATALOG_VERSION: u32 = 1;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Catalog {
    #[serde(deserialize_with = "deserialize_catalog_version")]
    schema_version: u32,
    #[serde(deserialize_with = "deserialize_items")]
    items: Vec<CatalogEntry>,
}

impl Catalog {
    pub(crate) fn items(&self) -> &[CatalogEntry] {
        &self.items
    }

    pub(crate) fn item(&self, id: Uuid) -> Option<&CatalogEntry> {
        self.items.iter().find(|item| item.id() == id)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CatalogEntry {
    id: NonNilUuid,
    #[serde(deserialize_with = "deserialize_non_empty_string")]
    name: String,
    #[serde(deserialize_with = "deserialize_non_empty_string")]
    version: String,
    kind: ItemKind,
    #[serde(deserialize_with = "deserialize_non_empty_vec")]
    artifacts: Vec<CatalogArtifact>,
}

impl CatalogEntry {
    pub(crate) fn id(&self) -> Uuid {
        self.id.get()
    }

    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    pub(crate) fn version(&self) -> &str {
        &self.version
    }

    pub(crate) fn kind(&self) -> ItemKind {
        self.kind
    }

    pub(crate) fn matching_artifacts(
        &self,
        target: Target,
    ) -> impl Iterator<Item = (usize, &CatalogArtifact)> {
        self.artifacts
            .iter()
            .enumerate()
            .filter(move |(_, artifact)| artifact.matches(target))
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "kebab-case", deny_unknown_fields)]
pub(crate) enum ItemKind {
    #[serde(rename = "runner")]
    RunnerComponent { flavour: RunnerKind },
    Addon {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        slot: Option<super::Slot>,
    },
    #[serde(rename = "internal")]
    InternalComponent { role: InternalRole },
}

impl ItemKind {
    pub(crate) fn is_single_artifact(self) -> bool {
        !matches!(self, Self::Addon { slot: None })
    }
}

pub(crate) fn category(kind: ItemKind) -> Option<&'static str> {
    match kind {
        ItemKind::RunnerComponent { .. } => Some("runners"),
        ItemKind::InternalComponent {
            role: InternalRole::Winebridge,
        } => Some("winebridge"),
        ItemKind::InternalComponent {
            role: InternalRole::Umu,
        } => Some("umu"),
        ItemKind::Addon {
            slot: Some(super::Slot::Dxvk),
        } => Some("dxvk"),
        ItemKind::Addon {
            slot: Some(super::Slot::Vkd3d),
        } => Some("vkd3d"),
        ItemKind::Addon {
            slot: Some(super::Slot::Nvapi),
        } => Some("nvapi"),
        ItemKind::Addon {
            slot: Some(super::Slot::LatencyFlex),
        } => Some("latency-flex"),
        ItemKind::Addon { slot: None } => None,
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum InternalRole {
    Umu,
    Winebridge,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CatalogArtifact {
    url: url::Url,
    #[serde(deserialize_with = "deserialize_non_empty_string")]
    file_name: String,
    #[serde(deserialize_with = "deserialize_checksum")]
    checksum: Checksum,
    #[serde(skip_serializing_if = "Option::is_none")]
    size: Option<std::num::NonZeroU64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    platform: Option<Target>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    guest_arch: Option<Architecture>,
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

    fn matches(&self, target: Target) -> bool {
        self.platform.is_none_or(|platform| platform == target)
    }
}

fn deserialize_catalog_version<'de, D>(deserializer: D) -> Result<u32, D::Error>
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

fn deserialize_items<'de, D>(deserializer: D) -> Result<Vec<CatalogEntry>, D::Error>
where
    D: Deserializer<'de>,
{
    let items = Vec::<CatalogEntry>::deserialize(deserializer)?;
    let mut ids = HashSet::new();
    for item in &items {
        if !ids.insert(item.id()) {
            return Err(de::Error::custom(format!(
                "duplicate catalog item id {}",
                item.id()
            )));
        }
        if item.kind.is_single_artifact() {
            for (index, artifact) in item.artifacts.iter().enumerate() {
                if item.artifacts[..index].iter().any(|other| {
                    artifact.platform.is_none()
                        || other.platform.is_none()
                        || artifact.platform == other.platform
                }) {
                    return Err(de::Error::custom(format!(
                        "catalog item {} has overlapping platform artifacts",
                        item.id()
                    )));
                }
            }
        } else {
            let mut file_names = HashSet::new();
            if item
                .artifacts
                .iter()
                .any(|artifact| !file_names.insert(&artifact.file_name))
            {
                return Err(de::Error::custom(format!(
                    "dependency addon {} has duplicate artifact file names",
                    item.id()
                )));
            }
        }
        for artifact in &item.artifacts {
            let mut components = Path::new(&artifact.file_name).components();
            if !matches!(components.next(), Some(Component::Normal(_)))
                || components.next().is_some()
            {
                return Err(de::Error::custom(format!(
                    "artifact file_name must be a single file name: {}",
                    artifact.file_name
                )));
            }
        }
    }
    Ok(items)
}

fn deserialize_checksum<'de, D>(deserializer: D) -> Result<Checksum, D::Error>
where
    D: Deserializer<'de>,
{
    let checksum = Checksum::deserialize(deserializer)?;
    if checksum.value().is_empty() {
        return Err(de::Error::custom("checksum cannot be empty"));
    }
    Ok(checksum)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_overlapping_runner_artifacts() {
        let error = serde_json::from_str::<Catalog>(
            r#"{
                "schema_version": 1,
                "items": [{
                    "id": "00000000-0000-0000-0000-000000000001",
                    "name": "Runner",
                    "version": "1",
                    "kind": { "type": "runner", "flavour": "wine" },
                    "artifacts": [
                        { "url": "https://example.test/a", "file_name": "a", "checksum": { "algorithm": "sha256", "value": "a" } },
                        { "url": "https://example.test/b", "file_name": "b", "checksum": { "algorithm": "sha256", "value": "b" } }
                    ]
                }]
            }"#,
        )
        .unwrap_err();

        assert!(error.to_string().contains("overlapping platform artifacts"));
    }

    #[test]
    fn round_trips_every_install_step() {
        let catalog: Catalog = serde_json::from_str(
            r#"{
                "schema_version": 1,
                "items": [{
                    "id": "00000000-0000-0000-0000-000000000001",
                    "name": "vcrun2022",
                    "version": "1",
                    "kind": { "type": "addon" },
                    "artifacts": [{
                        "url": "https://example.test/vcrun.exe",
                        "file_name": "vcrun.exe",
                        "checksum": { "algorithm": "sha256", "value": "abc" },
                        "guest_arch": "x86_64",
                        "steps": [
                            { "action": "copy", "destination": "drive_c/a.dll" },
                            { "action": "execute", "arguments": ["/quiet"] },
                            { "action": "extract", "destination": "drive_c/runtime" },
                            { "action": "register-dlls", "dlls": ["drive_c/a.dll"] },
                            { "action": "set-registry-value", "hive": "current-user", "key": "Software\\Test", "name": "Installed", "value": { "dword": 1 } },
                            { "action": "set-dll-overrides", "dlls": ["a"], "mode": "native-builtin" },
                            { "action": "set-environment", "name": "TEST", "value": "1" }
                        ]
                    }]
                }]
            }"#,
        )
        .unwrap();

        let encoded = serde_json::to_string(&catalog).unwrap();
        let decoded: Catalog = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded.items(), catalog.items());
    }
}
