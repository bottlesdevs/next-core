use super::Dependency;
use crate::catalog::{Architecture, Catalog, deserialize_supported_schema_version};
use serde::{Deserialize, Serialize};
use std::num::NonZeroU32;
use uuid::Uuid;

const CATALOG_VERSION: u32 = 1;

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct DependencyCatalog {
    #[serde(deserialize_with = "deserialize_supported_schema_version::<_, CATALOG_VERSION>")]
    schema_version: NonZeroU32,
    components: Vec<Dependency>,
}

#[derive(Debug, Clone)]
pub struct DependencyCatalogQuery<'catalog> {
    dependencies: &'catalog [Dependency],
    uuid: Option<Uuid>,
    name: Option<String>,
    version: Option<String>,
    arch: Option<Architecture>,
}

impl<'catalog> DependencyCatalogQuery<'catalog> {
    fn new(dependencies: &'catalog [Dependency]) -> Self {
        Self {
            dependencies,
            uuid: None,
            name: None,
            version: None,
            arch: None,
        }
    }

    pub fn uuid(mut self, uuid: Uuid) -> Self {
        self.uuid = Some(uuid);
        self
    }

    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    pub fn version(mut self, version: impl Into<String>) -> Self {
        self.version = Some(version.into());
        self
    }

    pub fn arch(mut self, arch: Architecture) -> Self {
        self.arch = Some(arch);
        self
    }

    pub fn iter(&self) -> impl DoubleEndedIterator<Item = &'catalog Dependency> + '_ {
        self.dependencies
            .iter()
            .filter(|dependency| self.matches(dependency))
    }

    pub fn first(&self) -> Option<&'catalog Dependency> {
        self.iter().next()
    }

    pub fn last(&self) -> Option<&'catalog Dependency> {
        self.iter().next_back()
    }

    pub fn count(&self) -> usize {
        self.iter().count()
    }

    pub fn is_empty(&self) -> bool {
        self.first().is_none()
    }

    fn matches(&self, dependency: &Dependency) -> bool {
        self.uuid
            .map(|uuid| dependency.uuid() == uuid)
            .unwrap_or(true)
            && self
                .name
                .as_deref()
                .map(|name| dependency.name() == name)
                .unwrap_or(true)
            && self
                .version
                .as_deref()
                .map(|version| dependency.version() == version)
                .unwrap_or(true)
            && self
                .arch
                .map(|arch| dependency.supports(arch))
                .unwrap_or(true)
    }
}

impl IntoIterator for DependencyCatalog {
    type IntoIter = std::vec::IntoIter<Dependency>;
    type Item = Dependency;

    fn into_iter(self) -> Self::IntoIter {
        self.components.into_iter()
    }
}

impl<'catalog> IntoIterator for &'catalog DependencyCatalog {
    type IntoIter = std::slice::Iter<'catalog, Dependency>;
    type Item = &'catalog Dependency;

    fn into_iter(self) -> Self::IntoIter {
        self.components.iter()
    }
}

impl Catalog for DependencyCatalog {
    type Item = Dependency;
    type Query<'catalog> = DependencyCatalogQuery<'catalog>;

    fn version(&self) -> std::num::NonZeroU32 {
        self.schema_version
    }

    fn iter(&self) -> impl ExactSizeIterator<Item = &Self::Item> + DoubleEndedIterator {
        self.components.iter()
    }

    fn query(&self) -> Self::Query<'_> {
        DependencyCatalogQuery::new(&self.components)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::{Uuid, uuid};

    fn vcrun_2022_id() -> Uuid {
        uuid!("00000000-0000-0000-0000-000000000001")
    }

    fn dxvk_runtime_id() -> Uuid {
        uuid!("00000000-0000-0000-0000-000000000002")
    }

    fn catalog() -> DependencyCatalog {
        serde_json::from_slice::<DependencyCatalog>(
            br#"{
                "schema_version": 1,
                "components": [
                    {
                        "id": "00000000-0000-0000-0000-000000000001",
                        "name": "vcrun2022",
                        "version": "14.38.33135",
                        "files": [
                            {
                                "file_name": "vcruntime140.dll",
                                "destination": "drive_c/windows/system32/vcruntime140.dll",
                                "url": "https://example.test/vcruntime140.dll",
                                "checksum": {
                                    "algorithm": "sha256",
                                    "value": "abc"
                                },
                                "size": 123456,
                                "arch": "x86_64"
                            }
                        ]
                    },
                    {
                        "id": "00000000-0000-0000-0000-000000000002",
                        "name": "dxvk-runtime",
                        "version": "2.4",
                        "artifacts": [
                            {
                                "file_name": "dxvk-runtime.tar.gz",
                                "url": "https://example.test/dxvk-runtime.tar.gz",
                                "checksum": {
                                    "algorithm": "sha512",
                                    "value": "def"
                                },
                                "size": 654321,
                                "arch": "x86_64"
                            }
                        ]
                    }
                ]
            }"#,
        )
        .unwrap()
    }

    #[test]
    fn iterates_catalog_dependencies() {
        let catalog = catalog();
        let ids = catalog.iter().map(Dependency::uuid).collect::<Vec<_>>();

        assert_eq!(ids, vec![vcrun_2022_id(), dxvk_runtime_id()]);
    }

    #[test]
    fn query_filters_dependencies_by_name_and_version() {
        let catalog = catalog();

        let dependency = catalog
            .query()
            .name("vcrun2022")
            .version("14.38.33135")
            .first()
            .unwrap();

        assert_eq!(dependency.uuid(), vcrun_2022_id());
    }

    #[test]
    fn query_filters_dependencies_by_uuid() {
        let catalog = catalog();

        let dependency = catalog.query().uuid(dxvk_runtime_id()).first().unwrap();

        assert_eq!(dependency.uuid(), dxvk_runtime_id());
    }

    #[test]
    fn query_filters_dependencies_by_architecture() {
        let catalog = catalog();

        assert_eq!(catalog.query().arch(Architecture::X86_64).count(), 2);
        assert!(catalog.query().arch(Architecture::X86).is_empty());
    }

    #[test]
    fn query_reports_when_no_dependencies_match() {
        let catalog = catalog();

        assert!(catalog.query().name("missing").is_empty());
    }

    #[test]
    fn rejects_unsupported_schema_version() {
        let result = serde_json::from_slice::<DependencyCatalog>(
            br#"{
                "schema_version": 2,
                "components": [
                    {
                        "id": "00000000-0000-0000-0000-000000000001",
                        "name": "vcrun2022",
                        "version": "14.38.33135",
                        "files": [
                            {
                                "file_name": "vcruntime140.dll",
                                "destination": "drive_c/windows/system32/vcruntime140.dll",
                                "url": "https://example.test/vcruntime140.dll",
                                "checksum": {
                                    "algorithm": "sha256",
                                    "value": "abc"
                                },
                                "size": 123456,
                                "arch": "x86_64"
                            }
                        ]
                    }
                ]
            }"#,
        );

        assert!(result.is_err());
    }
}
