#[cfg(feature = "fvs")]
use crate::{ProgramManager, environment::VirgoManager};

#[cfg(feature = "fvs")]
use std::path::PathBuf;
use std::sync::Arc;

use http_client::{HttpClient, ReqwestClient};
use url::Url;

use crate::{
    Addons, BottleManager, Context, Directories, Library, Plugins, Profiles, error::Result,
};

#[derive(Clone, Debug, Default)]
pub struct Config {
    #[cfg(feature = "fvs")]
    pub fvs2d: Option<PathBuf>,
    pub component_catalog: Option<Url>,
    pub dependency_catalog: Option<Url>,
}

pub struct Bottles {
    context: Context,
    bottles: BottleManager,
    #[cfg(feature = "fvs")]
    programs: ProgramManager,
    addons: Addons,
    library: Library,
    profiles: Profiles,
    plugins: Arc<Plugins>,
}

impl Bottles {
    pub async fn open(config: Config) -> Result<Self> {
        let Config {
            #[cfg(feature = "fvs")]
            fvs2d,
            component_catalog,
            dependency_catalog,
        } = config;
        #[cfg(not(feature = "fvs"))]
        let fvs2d = None;
        let directories = Directories::new().await?;
        let plugins = Arc::new(Plugins::open(&directories).await?);
        let profiles = Profiles::load(&directories, plugins.clone()).await?;
        let http_client: Arc<dyn HttpClient> =
            Arc::new(ReqwestClient::new().map_err(download_manager::error::Error::from)?);
        let context = Context::new(directories, http_client, fvs2d)?;
        let addons = Addons::load(context.clone(), component_catalog, dependency_catalog).await?;
        #[cfg(feature = "fvs")]
        let virgo = Arc::new(VirgoManager::new(context.clone(), addons.clone()));
        let bottles = BottleManager::load(
            context.clone(),
            addons.clone(),
            #[cfg(feature = "fvs")]
            virgo.clone(),
        )
        .await?;
        #[cfg(feature = "fvs")]
        let programs = ProgramManager::load(context.clone(), addons.clone(), virgo).await?;
        let library = Library::new(
            bottles.clone(),
            #[cfg(feature = "fvs")]
            programs.clone(),
            profiles.clone(),
            plugins.clone(),
        );

        Ok(Self {
            context,
            bottles,
            #[cfg(feature = "fvs")]
            programs,
            addons,
            library,
            profiles,
            plugins,
        })
    }

    /// Gracefully stops background services.
    ///
    /// Calling this method more than once is safe.
    pub async fn shutdown(&self) -> Result<()> {
        self.context.downloader().shutdown().await;
        Ok(())
    }

    pub fn bottles(&self) -> &BottleManager {
        &self.bottles
    }

    #[cfg(feature = "fvs")]
    pub fn programs(&self) -> &ProgramManager {
        &self.programs
    }

    pub fn directories(&self) -> &Directories {
        self.context.directories()
    }

    pub fn addons(&self) -> &Addons {
        &self.addons
    }

    /// Returns the aggregate installed-program library and search entry point.
    pub fn library(&self) -> &Library {
        &self.library
    }

    /// Returns the persisted application profiles.
    pub fn profiles(&self) -> &Profiles {
        &self.profiles
    }

    /// Returns installed plugin lifecycle management.
    pub fn plugins(&self) -> &Plugins {
        &self.plugins
    }

    /// Returns the HTTP transport shared by core services.
    pub fn http_client(&self) -> &Arc<dyn HttpClient> {
        self.context.http_client()
    }
}
