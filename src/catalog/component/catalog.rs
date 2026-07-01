use super::{Component, ComponentKind, RunnerKind};
use crate::catalog::{
    Catalog, CatalogItem, Target, deserialize_supported_schema_version,
    deserialize_unique_catalog_items,
};
use serde::{Deserialize, Serialize};
use std::num::NonZeroU32;
use uuid::Uuid;

const CATALOG_VERSION: u32 = 1;

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct ComponentCatalog {
    #[serde(deserialize_with = "deserialize_supported_schema_version::<_, CATALOG_VERSION>")]
    schema_version: NonZeroU32,
    #[serde(deserialize_with = "deserialize_unique_catalog_items")]
    components: Vec<Component>,
}

impl CatalogItem for Component {
    fn uuid(&self) -> Uuid {
        self.uuid()
    }
}

#[derive(Debug, Clone)]
pub struct ComponentCatalogQuery<'catalog> {
    components: &'catalog [Component],
    uuid: Option<Uuid>,
    kind: Option<ComponentKind>,
    version: Option<String>,
    target: Option<Target>,
}

impl<'catalog> ComponentCatalogQuery<'catalog> {
    fn new(components: &'catalog [Component]) -> Self {
        Self {
            components,
            uuid: None,
            kind: None,
            version: None,
            target: None,
        }
    }

    pub fn uuid(mut self, uuid: Uuid) -> Self {
        self.uuid = Some(uuid);
        self
    }

    pub fn kind(mut self, kind: ComponentKind) -> Self {
        self.kind = Some(kind);
        self
    }

    pub fn runner(self, kind: RunnerKind) -> Self {
        self.kind(ComponentKind::Runner { kind })
    }

    pub fn version(mut self, version: impl Into<String>) -> Self {
        self.version = Some(version.into());
        self
    }

    pub fn target(mut self, target: Target) -> Self {
        self.target = Some(target);
        self
    }

    pub fn iter(&self) -> impl DoubleEndedIterator<Item = &'catalog Component> + '_ {
        self.components
            .iter()
            .filter(|component| self.matches(component))
    }

    pub fn first(&self) -> Option<&'catalog Component> {
        self.iter().next()
    }

    pub fn last(&self) -> Option<&'catalog Component> {
        self.iter().next_back()
    }

    pub fn count(&self) -> usize {
        self.iter().count()
    }

    pub fn is_empty(&self) -> bool {
        self.first().is_none()
    }

    fn matches(&self, component: &Component) -> bool {
        self.uuid
            .map(|uuid| component.uuid() == uuid)
            .unwrap_or(true)
            && self
                .kind
                .map(|kind| component.kind() == kind)
                .unwrap_or(true)
            && self
                .version
                .as_deref()
                .map(|version| component.version() == version)
                .unwrap_or(true)
            && self
                .target
                .map(|target| component.supports(target))
                .unwrap_or(true)
    }
}

impl IntoIterator for ComponentCatalog {
    type IntoIter = std::vec::IntoIter<Component>;
    type Item = Component;

    fn into_iter(self) -> Self::IntoIter {
        self.components.into_iter()
    }
}

impl<'catalog> IntoIterator for &'catalog ComponentCatalog {
    type IntoIter = std::slice::Iter<'catalog, Component>;
    type Item = &'catalog Component;

    fn into_iter(self) -> Self::IntoIter {
        self.components.iter()
    }
}

impl Catalog for ComponentCatalog {
    type Item = Component;
    type Query<'catalog> = ComponentCatalogQuery<'catalog>;

    fn version(&self) -> NonZeroU32 {
        self.schema_version
    }

    fn iter(&self) -> impl ExactSizeIterator<Item = &Self::Item> + DoubleEndedIterator {
        self.components.iter()
    }

    fn query(&self) -> Self::Query<'_> {
        ComponentCatalogQuery::new(&self.components)
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

    fn catalog() -> ComponentCatalog {
        serde_json::from_slice::<ComponentCatalog>(
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
    fn queries_catalog_component_by_kind_and_version() {
        let catalog = catalog();

        let component = catalog
            .query()
            .kind(ComponentKind::Runner {
                kind: RunnerKind::Proton,
            })
            .version("9-1")
            .first()
            .unwrap();

        assert_eq!(component.uuid(), ge_proton_9_1_id());
    }

    #[test]
    fn query_filters_components_by_kind() {
        let catalog = catalog();

        let component = catalog.query().kind(ComponentKind::Dxvk).first().unwrap();

        assert_eq!(component.uuid(), dxvk_2_4_id());
    }

    #[test]
    fn query_filters_runner_components_by_runner_kind() {
        let catalog = catalog();

        let component = catalog
            .query()
            .runner(RunnerKind::Proton)
            .version("9-1")
            .first()
            .unwrap();

        assert_eq!(component.uuid(), ge_proton_9_1_id());
    }

    #[test]
    fn query_filters_components_by_uuid() {
        let catalog = catalog();

        let component = catalog.query().uuid(dxvk_2_4_id()).first().unwrap();

        assert_eq!(component.uuid(), dxvk_2_4_id());
    }

    #[test]
    fn query_filters_components_by_target() {
        let catalog = catalog();

        assert_eq!(catalog.query().target(Target::linux_x86_64()).count(), 2);
    }

    #[test]
    fn query_reports_when_no_components_match() {
        let catalog = catalog();

        assert!(catalog.query().kind(ComponentKind::Vkd3d).is_empty());
    }

    #[test]
    fn rejects_duplicate_component_ids() {
        let result = serde_json::from_slice::<ComponentCatalog>(
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
    fn rejects_unsupported_schema_version() {
        let result = serde_json::from_slice::<ComponentCatalog>(
            br#"{
                "schema_version": 2,
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
        );

        assert!(result.is_err());
    }

    #[test]
    fn decodes_catalog_from_json() {
        let catalog = serde_json::from_slice::<ComponentCatalog>(
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

        assert_eq!(catalog.version().get(), 1);
        assert_eq!(
            catalog.iter().next().map(Component::uuid),
            Some(dxvk_2_4_id())
        );
    }
}
