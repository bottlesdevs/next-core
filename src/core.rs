use std::{path::PathBuf, sync::Arc};

use download_manager::manager::DownloadManager;
use url::Url;

use crate::{BottleManager, Context, Library, Paths, error::Result};

#[derive(Clone)]
pub struct Core {
    bottles: BottleManager,
    library: Library,
}

#[must_use]
pub struct CoreBuilder {
    paths: Paths,
    downloads: Arc<DownloadManager>,
    fvs2d: Option<PathBuf>,
    component_catalog: Option<Url>,
    dependency_catalog: Option<Url>,
}

impl Core {
    pub fn builder(paths: Paths, downloads: Arc<DownloadManager>) -> CoreBuilder {
        CoreBuilder {
            paths,
            downloads,
            fvs2d: None,
            component_catalog: None,
            dependency_catalog: None,
        }
    }

    pub fn bottles(&self) -> &BottleManager {
        &self.bottles
    }

    pub fn library(&self) -> &Library {
        &self.library
    }
}

impl CoreBuilder {
    pub fn fvs2d(mut self, executable: impl Into<PathBuf>) -> Self {
        self.fvs2d = Some(executable.into());
        self
    }

    pub fn component_catalog(mut self, url: Url) -> Self {
        self.component_catalog = Some(url);
        self
    }

    pub fn dependency_catalog(mut self, url: Url) -> Self {
        self.dependency_catalog = Some(url);
        self
    }

    pub async fn build(self) -> Result<Core> {
        let Self {
            paths,
            downloads,
            fvs2d,
            component_catalog,
            dependency_catalog,
        } = self;
        let context = Context::new(paths.clone(), fvs2d)?;
        let library =
            Library::load(paths, component_catalog, dependency_catalog, downloads).await?;
        context.set_library(library.clone());
        Ok(Core {
            bottles: BottleManager::new(context),
            library,
        })
    }
}
