use super::{Component, ComponentKind};
use serde::{Deserialize, Deserializer, Serialize, de};
use std::{collections::HashSet, num::NonZeroU32};

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct Catalog {
    schema_version: NonZeroU32,
    #[serde(deserialize_with = "deserialize_unique_components")]
    components: Vec<Component>,
}

impl Catalog {
    pub fn schema_version(&self) -> u32 {
        self.schema_version.get()
    }

    pub fn components(&self) -> &[Component] {
        &self.components
    }

    pub fn iter(&self) -> impl ExactSizeIterator<Item = &Component> + DoubleEndedIterator {
        self.components.iter()
    }

    pub fn find(&self, kind: ComponentKind, version: &str) -> Option<&Component> {
        self.iter()
            .filter(|component| component.kind() == kind)
            .find(|component| component.version() == version)
    }
}

fn deserialize_unique_components<'de, D>(deserializer: D) -> Result<Vec<Component>, D::Error>
where
    D: Deserializer<'de>,
{
    let components = Vec::<Component>::deserialize(deserializer)?;
    let mut keys = HashSet::new();

    for component in &components {
        if !keys.insert(component.uuid()) {
            return Err(de::Error::custom(format!(
                "duplicate component id {}",
                component.uuid()
            )));
        }
    }

    Ok(components)
}

impl IntoIterator for Catalog {
    type IntoIter = std::vec::IntoIter<Component>;
    type Item = Component;

    fn into_iter(self) -> Self::IntoIter {
        self.components.into_iter()
    }
}

impl<'catalog> IntoIterator for &'catalog Catalog {
    type IntoIter = std::slice::Iter<'catalog, Component>;
    type Item = &'catalog Component;

    fn into_iter(self) -> Self::IntoIter {
        self.components.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::component::RunnerKind;
    use uuid::{Uuid, uuid};

    fn dxvk_2_4_id() -> Uuid {
        uuid!("00000000-0000-0000-0000-000000000001")
    }

    fn ge_proton_9_1_id() -> Uuid {
        uuid!("00000000-0000-0000-0000-000000000002")
    }

    fn catalog() -> Catalog {
        serde_json::from_slice::<Catalog>(
            br#"{
                "schema_version": 1,
                "components": [
                    {
                        "id": "00000000-0000-0000-0000-000000000001",
                        "version": "2.4",
                        "kind": {
                            "type": "dxvk"
                        },
                        "artifacts": [
                            {
                                "url": "https://example.test/dxvk-2.4.tar.gz",
                                "file_name": "dxvk-2.4.tar.gz",
                                "checksum": {
                                    "algorithm": "sha256",
                                    "value": "abc"
                                }
                            }
                        ]
                    },
                    {
                        "id": "00000000-0000-0000-0000-000000000002",
                        "version": "9-1",
                        "kind": {
                            "type": "runner",
                            "runner": "proton"
                        },
                        "artifacts": [
                            {
                                "url": "https://example.test/ge-proton-9-1.tar.gz",
                                "file_name": "ge-proton-9-1.tar.gz",
                                "checksum": {
                                    "algorithm": "sha256",
                                    "value": "abc"
                                }
                            }
                        ]
                    }
                ]
            }"#,
        )
        .unwrap()
    }

    #[test]
    fn iterates_catalog_components() {
        let catalog = catalog();
        let ids = catalog.iter().map(Component::uuid).collect::<Vec<_>>();

        assert_eq!(ids, vec![dxvk_2_4_id(), ge_proton_9_1_id()]);
    }

    #[test]
    fn finds_catalog_component_by_kind_and_version() {
        let catalog = catalog();

        let component = catalog
            .find(
                ComponentKind::Runner {
                    kind: RunnerKind::Proton,
                },
                "9-1",
            )
            .unwrap();

        assert_eq!(component.uuid(), ge_proton_9_1_id());
    }

    #[test]
    fn rejects_duplicate_component_ids() {
        let result = serde_json::from_slice::<Catalog>(
            br#"{
                "schema_version": 1,
                "components": [
                    {
                        "id": "00000000-0000-0000-0000-000000000001",
                        "version": "2.4",
                        "kind": {
                            "type": "dxvk"
                        },
                        "artifacts": [
                            {
                                "url": "https://example.test/dxvk-a.tar.gz",
                                "file_name": "dxvk-a.tar.gz",
                                "checksum": {
                                    "algorithm": "sha256",
                                    "value": "abc"
                                }
                            }
                        ]
                    },
                    {
                        "id": "00000000-0000-0000-0000-000000000001",
                        "version": "2.5",
                        "kind": {
                            "type": "dxvk"
                        },
                        "artifacts": [
                            {
                                "url": "https://example.test/dxvk-b.tar.gz",
                                "file_name": "dxvk-b.tar.gz",
                                "checksum": {
                                    "algorithm": "sha256",
                                    "value": "abc"
                                }
                            }
                        ]
                    }
                ]
            }"#,
        );

        assert!(result.is_err());
    }

    #[test]
    fn decodes_catalog_from_json() {
        let catalog = serde_json::from_slice::<Catalog>(
            br#"{
                "schema_version": 1,
                "components": [
                    {
                        "id": "00000000-0000-0000-0000-000000000001",
                        "version": "2.4",
                        "kind": {
                            "type": "dxvk"
                        },
                        "artifacts": [
                            {
                                "url": "https://example.test/dxvk-2.4.tar.gz",
                                "file_name": "dxvk-2.4.tar.gz",
                                "checksum": {
                                    "algorithm": "sha256",
                                    "value": "abc"
                                }
                            }
                        ]
                    }
                ]
            }"#,
        )
        .unwrap();

        assert_eq!(catalog.schema_version(), 1);
        assert_eq!(catalog.components()[0].uuid(), dxvk_2_4_id());
    }
}
