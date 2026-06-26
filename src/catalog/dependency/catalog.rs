use super::Dependency;
use crate::catalog::Catalog;
use serde::{Deserialize, Serialize};
use std::num::NonZeroU32;

static CATALOG_VERSION: NonZeroU32 = NonZeroU32::new(1).unwrap();

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct DependencyCatalog {
    schema_version: NonZeroU32,
    components: Vec<Dependency>,
}

pub struct DependencyCatalogQuery {}

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
    type Query = DependencyCatalogQuery;

    fn version(&self) -> std::num::NonZeroU32 {
        CATALOG_VERSION
    }

    fn iter(&self) -> impl ExactSizeIterator<Item = &Self::Item> + DoubleEndedIterator {
        self.components.iter()
    }

    fn query(&self) -> Self::Query {
        todo!()
    }
}
