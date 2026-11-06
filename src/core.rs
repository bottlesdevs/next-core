//! Initialization and shared services for a Bottles application.

use std::sync::Arc;

use http_client::{HttpClient, ReqwestClient};
use url::Url;

use crate::{Addons, Context, Directories, Prefixes, error::Result};

/// Startup options for [`Bottles::open`].
///
/// Catalog URLs are used by [`Addons::refresh`](crate::Addons::refresh); cached
/// catalogs and installed addons still load when a URL is absent. Refreshing a
/// family without a configured URL reports an error for that family. With the
/// `fvs` feature enabled, an unset daemon path resolves `fvs2d` through `PATH`.
#[derive(Clone, Debug, Default)]
pub struct Config {
    /// Explicit path to the `fvs2d` executable, or `None` to resolve it through
    /// `PATH`. Relative paths are resolved against the process's current directory.
    #[cfg(feature = "fvs")]
    pub fvs2d: Option<std::path::PathBuf>,
    /// Remote runner, WineBridge, UMU, and prefix-component catalog downloaded by
    /// [`Addons::refresh`](crate::Addons::refresh).
    pub component_catalog: Option<Url>,
    /// Remote dependency catalog downloaded by
    /// [`Addons::refresh`](crate::Addons::refresh).
    pub dependency_catalog: Option<Url>,
}

/// Owns the services and persisted collections used by Bottles Next.
///
/// Create one instance with [`Bottles::open`], borrow its collections for the
/// lifetime of the application, then call [`Bottles::shutdown`] after all
/// outstanding [`Operation`](crate::Operation) values have completed.
pub struct Bottles {
    context: Context,
    prefixes: Prefixes,
}

impl Bottles {
    /// Opens the application core and loads persisted state.
    ///
    /// This resolves application directories, loads cached addon
    /// catalogs, installed addons, and prefix registries.
    /// With the `fvs` feature, it connects to or starts `fvs2d` before loading
    /// Virgo-backed prefixes.
    ///
    /// # Errors
    ///
    /// Returns an error if application directories are unavailable; persisted
    /// addons or prefixes cannot be read; the HTTP
    /// or download service cannot initialize; an FVS executable cannot be
    /// resolved or contacted.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use bottles_core::{Bottles, Config};
    /// # async fn example() -> Result<(), bottles_core::error::Error> {
    /// let core = Bottles::open(Config::default()).await?;
    /// core.shutdown().await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn open(config: Config) -> Result<Self> {
        let directories = Directories::new().await?;
        let http_client: Arc<dyn HttpClient> =
            Arc::new(ReqwestClient::new().map_err(download_manager::error::Error::from)?);
        let context = Context::new(
            #[cfg(feature = "fvs")]
            crate::virgo::Fvs::connect(config.fvs2d, &directories).await?,
            directories,
            http_client,
            config.component_catalog,
            config.dependency_catalog,
        )
        .await?;
        let prefixes = Prefixes::load(context.directories().prefixes(), context.clone()).await?;
        Ok(Self { context, prefixes })
    }

    /// Cancels downloads and stops the background download service.
    ///
    /// Stop submitting work and finish or cooperatively cancel and await
    /// outstanding operations before shutdown. Shutdown cancels queued and
    /// in-flight downloads, waits for their workers to exit, and prevents later
    /// download requests from being accepted. It does not stop Wine processes;
    /// call [`Prefix::stop`](crate::Prefix::stop) explicitly. With `fvs`
    /// enabled, the connected or spawned `fvs2d` daemon remains running for later
    /// clients. Calling shutdown more than once is safe.
    ///
    /// # Errors
    ///
    /// Shutdown is infallible.
    pub async fn shutdown(&self) -> Result<()> {
        self.context.downloader().shutdown().await;
        Ok(())
    }

    /// Returns the collection of persisted [`Prefix`](crate::Prefix) handles.
    pub fn prefixes(&self) -> &Prefixes {
        &self.prefixes
    }

    /// Returns the resolved application directories.
    pub fn directories(&self) -> &Directories {
        self.context.directories()
    }

    /// Returns the shared addon catalog and installation manager.
    pub fn addons(&self) -> &Addons {
        self.context.addons()
    }

    /// Returns the HTTP transport shared by core services.
    pub fn http_client(&self) -> &Arc<dyn HttpClient> {
        self.context.http_client()
    }
}
