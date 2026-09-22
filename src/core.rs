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
    library: Library,
    profiles: Profiles,
    plugins: Arc<Plugins>,
}

impl Bottles {
    /// Open core services and local state. With FVS enabled, connect to or start its
    /// daemon before opening owner registries; connection failures abort startup.
    pub async fn open(config: Config, plugins: Arc<Plugins>) -> Result<Self> {
        let Config {
            #[cfg(feature = "fvs")]
            fvs2d,
            component_catalog,
            dependency_catalog,
        } = config;
        let directories = Directories::new().await?;
        let profiles = Profiles::load(&directories, plugins.clone()).await?;
        let http_client: Arc<dyn HttpClient> =
            Arc::new(ReqwestClient::new().map_err(download_manager::error::Error::from)?);
        #[cfg(feature = "fvs")]
        let fvs = Arc::new(
            fvs_rs::Fvs2dClient::connect_or_spawn(
                fvs_executable(fvs2d)?,
                directories.runtime_dir().join("fvs2d.sock"),
            )
            .await?,
        );
        let context = Context::new(
            directories,
            http_client,
            #[cfg(feature = "fvs")]
            fvs,
            component_catalog,
            dependency_catalog,
        )
        .await?;
        #[cfg(feature = "fvs")]
        let virgo = Arc::new(VirgoManager::new(context.clone()));
        let bottles = BottleManager::load(
            context.clone(),
            #[cfg(feature = "fvs")]
            virgo.clone(),
        )
        .await?;
        #[cfg(feature = "fvs")]
        let programs = ProgramManager::load(context.clone(), virgo).await?;
        let library = Library::new(
            bottles.clone(),
            #[cfg(feature = "fvs")]
            programs.clone(),
            profiles.clone(),
        );

        Ok(Self {
            context,
            bottles,
            #[cfg(feature = "fvs")]
            programs,
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
        self.context.addons()
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
    pub fn plugins(&self) -> &Arc<Plugins> {
        &self.plugins
    }

    /// Returns the HTTP transport shared by core services.
    pub fn http_client(&self) -> &Arc<dyn HttpClient> {
        self.context.http_client()
    }
}

#[cfg(feature = "fvs")]
fn fvs_executable(configured: Option<PathBuf>) -> Result<PathBuf> {
    Ok(configured
        .map(crate::utils::absolute_path)
        .transpose()?
        .unwrap_or_else(|| PathBuf::from("fvs2d")))
}

#[cfg(all(test, feature = "fvs"))]
mod tests {
    use super::*;

    #[test]
    fn uses_path_lookup_when_fvs2d_is_not_configured() {
        assert_eq!(fvs_executable(None).unwrap(), PathBuf::from("fvs2d"));
    }
}
