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
        let bottles = BottleManager::load(context.clone(), addons.clone()).await?;
        let library = Library::new(bottles.clone(), profiles.clone(), plugins.clone());

        Ok(Self {
            context,
            bottles,
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

    /// Rebuilds the shared Virgo base using the latest catalog Soda release.
    /// That exact release must already be downloaded. Existing runtimes, snapshots,
    /// and completed addon caches remain intact; stopped preparations adopt the base.
    #[cfg(feature = "fvs")]
    pub fn rebuild_virgo_base(&self) -> crate::Operation<()> {
        let cx = self.context.clone();
        let addons = self.addons.clone();
        crate::Operation::new(move |progress, cancellation| async move {
            progress.send_replace(Some(crate::Progress::new(crate::Stage::CreatingPrefix)));
            crate::environment::artifacts::rebuild_base(&addons, &cx, &cancellation).await
        })
    }

    pub fn bottles(&self) -> &BottleManager {
        &self.bottles
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
