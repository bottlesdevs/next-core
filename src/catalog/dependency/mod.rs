mod catalog;
mod dependency;

pub use catalog::{DependencyCatalog, DependencyCatalogQuery};
pub use dependency::{Dependency, DependencyArtifact, DependencyFile, DependencySource};
