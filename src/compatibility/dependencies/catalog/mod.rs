#[allow(clippy::module_inception)]
mod catalog;
mod manifest;

pub use catalog::{DependencyCatalog, DependencyCatalogQuery};
pub use manifest::{CatalogDependencyEntry, DependencyResource};
