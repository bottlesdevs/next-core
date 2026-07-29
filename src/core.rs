use std::{path::PathBuf, sync::Arc};

use download_manager::manager::DownloadManager;
use url::Url;

use crate::{BottleManager, Context, Library, Paths, error::Result};

#[derive(Clone)]
pub struct Core {
    bottles: BottleManager,
    library: Arc<Library>,
}

impl Core {
    pub async fn open(
        paths: Paths,
        fvs2d: impl Into<PathBuf>,
        component_catalog_url: Url,
        dependency_catalog_url: Url,
        downloads: Arc<DownloadManager>,
    ) -> Result<Self> {
        let context = Context::new(paths.clone(), fvs2d)?;
        let library = Library::load(
            paths,
            component_catalog_url,
            dependency_catalog_url,
            downloads,
        )
        .await?;
        context.set_library(library.clone());
        Ok(Self {
            bottles: BottleManager::new(context.clone()),
            library,
        })
    }

    pub fn bottles(&self) -> &BottleManager {
        &self.bottles
    }

    pub fn library(&self) -> &Library {
        &self.library
    }
}
