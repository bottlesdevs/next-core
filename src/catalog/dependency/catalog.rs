use super::Dependency;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct Catalog {
    schema_version: u32,
    components: Vec<Dependency>,
}

impl Catalog {
    pub fn iter(&self) -> impl ExactSizeIterator<Item = &Dependency> + DoubleEndedIterator {
        self.components.iter()
    }
}

impl IntoIterator for Catalog {
    type IntoIter = std::vec::IntoIter<Dependency>;
    type Item = Dependency;

    fn into_iter(self) -> Self::IntoIter {
        self.components.into_iter()
    }
}

impl<'catalog> IntoIterator for &'catalog Catalog {
    type IntoIter = std::slice::Iter<'catalog, Dependency>;
    type Item = &'catalog Dependency;

    fn into_iter(self) -> Self::IntoIter {
        self.components.iter()
    }
}
