use serde::{Deserialize, Serialize};
use url::Url;
use uuid::{NonNilUuid, Uuid};

use crate::compatibility::{
    Checksum, Target,
    catalog::{Artifact, Catalog, CatalogItem},
    deserialize_non_empty_string, deserialize_non_empty_vec,
};

const CATALOG_VERSION: u32 = 1;

pub type ComponentCatalog = Catalog<CatalogComponentEntry, CATALOG_VERSION>;

impl CatalogItem for CatalogComponentEntry {
    fn uuid(&self) -> Uuid {
        self.uuid()
    }
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct CatalogComponentEntry {
    id: NonNilUuid,
    #[serde(deserialize_with = "deserialize_non_empty_string")]
    version: String,
    kind: ComponentKind,
    #[serde(deserialize_with = "deserialize_non_empty_vec")]
    artifacts: Vec<ComponentArtifact>,
}

impl CatalogComponentEntry {
    pub fn uuid(&self) -> Uuid {
        self.id.get()
    }

    pub fn version(&self) -> &str {
        &self.version
    }

    pub fn kind(&self) -> ComponentKind {
        self.kind
    }

    pub fn artifacts(&self) -> &[ComponentArtifact] {
        &self.artifacts
    }

    pub fn artifact_for(&self, target: Target) -> Option<&ComponentArtifact> {
        self.artifacts()
            .iter()
            .find(|artifact| artifact.target() == Some(target))
            .or_else(|| {
                self.artifacts()
                    .iter()
                    .find(|artifact| artifact.target().is_none())
            })
    }

    pub fn supports(&self, target: Target) -> bool {
        self.artifacts()
            .iter()
            .any(|artifact| artifact.matches(target))
    }
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct ComponentArtifact {
    #[serde(flatten)]
    artifact: Artifact,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    target: Option<Target>,
}

impl ComponentArtifact {
    pub fn file_name(&self) -> &str {
        &self.artifact.file_name()
    }

    pub fn url(&self) -> &Url {
        &self.artifact.url()
    }

    pub fn checksum(&self) -> &Checksum {
        &self.artifact.checksum()
    }

    pub fn size(&self) -> Option<u64> {
        self.artifact.size()
    }

    pub fn target(&self) -> Option<Target> {
        self.target
    }

    pub fn matches(&self, target: Target) -> bool {
        match self.target {
            Some(artifact_target) => artifact_target == target,
            None => true,
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum ComponentKind {
    Winebridge,
    Umu,
    Dxvk,
    Vkd3d,
    Nvapi,
    LatencyFlex,
    Runner {
        #[serde(rename = "runner")]
        kind: RunnerKind,
    },
}

impl ComponentKind {
    pub fn is_runner(self) -> bool {
        matches!(self, Self::Runner { .. })
    }

    pub fn runner_kind(self) -> Option<RunnerKind> {
        match self {
            Self::Runner { kind: runner_kind } => Some(runner_kind),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum RunnerKind {
    Wine,
    Proton,
}

#[cfg(test)]
mod tests {
    use uuid::uuid;

    use super::*;

    #[test]
    fn deserializes_runner_kind_from_typed_kind() {
        let component: CatalogComponentEntry = serde_json::from_str(
            r#"{
                "id": "00000000-0000-0000-0000-000000000002",
                "version": "1",
                "kind": {
                    "type": "runner",
                    "runner": "proton"
                },
                "artifacts": [
                    {
                        "url": "https://example.test/ge-proton-1.tar.gz",
                        "file_name": "ge-proton-1.tar.gz",
                        "checksum": {
                            "algorithm": "sha256",
                            "value": "abc"
                        },
                        "size": 42,
                        "target": {
                            "os": "linux",
                            "arch": "x86_64"
                        }
                    }
                ]
            }"#,
        )
        .unwrap();

        assert_eq!(
            component.kind(),
            ComponentKind::Runner {
                kind: RunnerKind::Proton
            }
        );
        assert_eq!(component.artifacts()[0].size(), Some(42));
        assert_eq!(
            component.artifact_for(Target::linux_x86_64()),
            component.artifacts().first()
        );
    }

    #[test]
    fn generic_artifacts_match_any_target() {
        let artifact = ComponentArtifact {
            artifact: Artifact {
                url: Url::parse("https://example.test/dxvk.tar.gz").unwrap(),
                file_name: String::from("dxvk.tar.gz"),
                checksum: Checksum::sha256("abc"),
                size: None,
            },
            target: None,
        };

        assert!(artifact.matches(Target::linux_x86_64()));
    }

    #[test]
    fn artifact_lookup_prefers_exact_target_over_generic_artifact() {
        let generic = ComponentArtifact {
            artifact: Artifact {
                url: Url::parse("https://example.test/generic.tar.gz").unwrap(),
                file_name: String::from("generic.tar.gz"),
                checksum: Checksum::sha256("abc"),
                size: None,
            },
            target: None,
        };
        let linux = ComponentArtifact {
            artifact: Artifact {
                url: Url::parse("https://example.test/linux.tar.gz").unwrap(),
                file_name: String::from("linux.tar.gz"),
                checksum: Checksum::sha256("def"),
                size: None,
            },
            target: Some(Target::linux_x86_64()),
        };
        let component = CatalogComponentEntry {
            id: NonNilUuid::new(uuid!("00000000-0000-0000-0000-ffff00000000")).unwrap(),
            version: String::from("2.4"),
            kind: ComponentKind::Dxvk,
            artifacts: vec![generic, linux],
        };

        assert_eq!(
            component
                .artifact_for(Target::linux_x86_64())
                .map(ComponentArtifact::file_name),
            Some("linux.tar.gz")
        );
    }
}
