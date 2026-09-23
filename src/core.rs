//! Initialization and shared services for a Bottles application.
//!
//! [`Bottles`] is the root object for this crate. It loads persisted state and
//! plugins once, then provides borrowed access to the managers that share that
//! state.

#[cfg(feature = "fvs")]
use crate::{Program, environment::VirgoManager};

use bottles_plugin_host::{PluginInterface, Plugins};
#[cfg(feature = "fvs")]
use std::path::PathBuf;
use std::sync::Arc;

use http_client::{HttpClient, ReqwestClient};
use url::Url;

use crate::{Addons, Bottle, Context, Directories, Library, Manager, Profiles, error::Result};

/// Startup options for [`Bottles::open`].
///
/// Leaving a catalog URL unset keeps that catalog local-only. With the `fvs`
/// feature enabled, an unset daemon path uses `fvs2d` from `PATH`.
///
/// # Examples
///
/// ```
/// use bottles_core::Config;
///
/// let config = Config::default();
/// assert!(config.component_catalog.is_none());
/// assert!(config.dependency_catalog.is_none());
/// ```
#[derive(Clone, Debug, Default)]
pub struct Config {
    /// Explicit path to the `fvs2d` executable, or `None` to search `PATH`.
    #[cfg(feature = "fvs")]
    pub fvs2d: Option<PathBuf>,
    /// Remote component catalog used by [`Addons::refresh`](crate::Addons::refresh).
    pub component_catalog: Option<Url>,
    /// Remote dependency catalog used by [`Addons::refresh`](crate::Addons::refresh).
    pub dependency_catalog: Option<Url>,
}

/// Owns the services and persisted collections used by Bottles Next.
///
/// Create one instance with [`Bottles::open`], borrow its managers for the
/// lifetime of the application, then call [`Bottles::shutdown`] after all
/// outstanding [`Operation`](crate::Operation) values have completed.
///
/// # Examples
///
/// ```no_run
/// # use std::sync::Arc;
/// # use bottles_core::{Bottles, Config};
/// # use bottles_plugin_host::Plugins;
/// # async fn example(plugins: Arc<Plugins>) -> Result<(), bottles_core::error::Error> {
/// let core = Bottles::open(Config::default(), plugins).await?;
/// println!("{} bottles", core.bottles().list().len());
/// core.shutdown().await?;
/// # Ok(())
/// # }
/// ```
pub struct Bottles {
    context: Context,
    bottles: Manager<Bottle>,
    #[cfg(feature = "fvs")]
    programs: Manager<Program>,
    library: Library,
    profiles: Profiles,
}

impl Bottles {
    /// Opens the application core and loads persisted state.
    ///
    /// This discovers application directories, loads profiles and environment
    /// registries, initializes addon catalogs, and registers native and plugin
    /// library providers. With the `fvs` feature, it also connects to or starts
    /// `fvs2d` before loading Virgo-backed environments.
    ///
    /// # Errors
    ///
    /// Returns an error if application directories are unavailable, persisted
    /// configuration cannot be read, a required service cannot start, an FVS
    /// connection fails, or an exported plugin provider cannot be loaded.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use std::sync::Arc;
    /// # use bottles_core::{Bottles, Config};
    /// # use bottles_plugin_host::Plugins;
    /// # async fn example(plugins: Arc<Plugins>) -> Result<(), bottles_core::error::Error> {
    /// let core = Bottles::open(Config::default(), plugins).await?;
    /// assert!(!core.profiles().selected().name().is_empty());
    /// core.shutdown().await?;
    /// # Ok(())
    /// # }
    /// ```
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
        let bottles = Manager::<Bottle>::load(
            context.directories().bottles(),
            context.clone(),
            #[cfg(feature = "fvs")]
            virgo.clone(),
        )
        .await?;
        #[cfg(feature = "fvs")]
        let programs =
            Manager::<Program>::load(context.directories().programs(), context.clone(), virgo)
                .await?;
        let library = Library::default();
        library.register_provider(Arc::new(bottles.clone()));
        #[cfg(feature = "fvs")]
        library.register_provider(Arc::new(programs.clone()));
        for plugin in plugins.list() {
            if plugin.exports(PluginInterface::LibraryProvider) {
                library.register_provider(Arc::new(plugins.load(&plugin.manifest.id).await?));
            }
        }

        Ok(Self {
            context,
            bottles,
            #[cfg(feature = "fvs")]
            programs,
            library,
            profiles,
        })
    }

    /// Stops the background download service.
    ///
    /// Stop submitting work and finish or cooperatively cancel and await outstanding
    /// operations before shutdown. Their execution belongs to the caller.
    /// Calling this method more than once is safe.
    ///
    /// # Errors
    ///
    /// This implementation currently always succeeds; the [`Result`](crate::error::Result)
    /// return type allows shutdown failures to be added without changing the API.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use bottles_core::Bottles;
    /// # async fn example(core: &Bottles) -> Result<(), bottles_core::error::Error> {
    /// core.shutdown().await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn shutdown(&self) -> Result<()> {
        self.context.downloader().shutdown().await;
        Ok(())
    }

    /// Returns the manager for persisted [`Bottle`] environments.
    ///
    /// # Examples
    ///
    /// ```
    /// # use bottles_core::Bottles;
    /// # fn example(core: &Bottles) {
    /// let known_bottles = core.bottles().list();
    /// # let _ = known_bottles;
    /// # }
    /// ```
    pub fn bottles(&self) -> &Manager<Bottle> {
        &self.bottles
    }

    #[cfg(feature = "fvs")]
    /// Returns the manager for standalone Virgo [`Program`] environments.
    ///
    /// # Examples
    ///
    /// ```
    /// # use bottles_core::Bottles;
    /// # fn example(core: &Bottles) {
    /// let known_programs = core.programs().list();
    /// # let _ = known_programs;
    /// # }
    /// ```
    pub fn programs(&self) -> &Manager<Program> {
        &self.programs
    }

    /// Returns the resolved application directories.
    ///
    /// # Examples
    ///
    /// ```
    /// # use bottles_core::Bottles;
    /// # fn example(core: &Bottles) {
    /// println!("{}", core.directories().data_dir().display());
    /// # }
    /// ```
    pub fn directories(&self) -> &Directories {
        self.context.directories()
    }

    /// Returns the shared addon catalog and installation manager.
    ///
    /// # Examples
    ///
    /// ```
    /// # use bottles_core::Bottles;
    /// # fn example(core: &Bottles) {
    /// println!("{} components", core.addons().components().len());
    /// # }
    /// ```
    pub fn addons(&self) -> &Addons {
        self.context.addons()
    }

    /// Returns the registry of installed, launchable library providers.
    ///
    /// # Examples
    ///
    /// ```
    /// # use bottles_core::Bottles;
    /// # async fn example(core: &Bottles) -> Result<(), bottles_core::error::Error> {
    /// let entries = core.library().list().await?;
    /// # let _ = entries;
    /// # Ok(())
    /// # }
    /// ```
    pub fn library(&self) -> &Library {
        &self.library
    }

    /// Returns the persisted application profiles.
    ///
    /// # Examples
    ///
    /// ```
    /// # use bottles_core::Bottles;
    /// # fn example(core: &Bottles) {
    /// println!("{}", core.profiles().selected().name());
    /// # }
    /// ```
    pub fn profiles(&self) -> &Profiles {
        &self.profiles
    }

    /// Returns the HTTP transport shared by core services.
    ///
    /// # Examples
    ///
    /// ```
    /// # use bottles_core::Bottles;
    /// # fn example(core: &Bottles) {
    /// let client = core.http_client().clone();
    /// # let _ = client;
    /// # }
    /// ```
    pub fn http_client(&self) -> &Arc<dyn HttpClient> {
        self.context.http_client()
    }
}

#[cfg(feature = "fvs")]
fn fvs_executable(configured: Option<PathBuf>) -> Result<PathBuf> {
    Ok(configured
        .map(crate::utils::fs::absolute_path)
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
