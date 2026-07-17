#[allow(clippy::module_inception)]
mod catalog;
mod manifest;

pub use catalog::{ComponentCatalog, ComponentCatalogQuery};
pub use manifest::{CatalogComponentEntry, ComponentArtifact, ComponentKind, RunnerKind};
