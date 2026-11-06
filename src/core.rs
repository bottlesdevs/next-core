//! Initialization and shared services for a Bottles application.

use std::sync::Arc;

use http_client::{HttpClient, ReqwestClient};
use url::Url;

use crate::{Addons, Context, Prefixes, error::Result, utils::paths};

/// Owns the services and persisted collections used by Bottles Next.
///
/// Create one instance with [`Core::builder`], borrow its collections for the
/// lifetime of the application, then call [`Core::shutdown`] after all
/// outstanding [`Operation`](crate::Operation) values have completed.
pub struct Core {
    context: Context,
    prefixes: Prefixes,
}

/// Startup options for a [`Core`], opened with [`CoreBuilder::build`].
///
/// Every option is optional. Catalog URLs are used by
/// [`Addons::refresh`](crate::Addons::refresh); cached catalogs and installed
/// addons still load when a URL is absent, and refreshing a family without a
/// URL reports an error for that family.
#[must_use = "call `build` to open the core"]
#[derive(Default)]
pub struct CoreBuilder {
    http_client: Option<Arc<dyn HttpClient>>,
    #[cfg(feature = "fvs")]
    fvs2d: Option<std::path::PathBuf>,
    component_catalog: Option<Url>,
    dependency_catalog: Option<Url>,
}

impl Core {
    /// Returns a builder with the default startup options.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use bottles_core::Core;
    /// # async fn example() -> Result<(), bottles_core::error::Error> {
    /// let core = Core::builder().build().await?;
    /// core.shutdown().await?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn builder() -> CoreBuilder {
        CoreBuilder::default()
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

    /// Returns the shared addon catalog and installation manager.
    pub fn addons(&self) -> &Addons {
        self.context.addons()
    }
}

impl CoreBuilder {
    /// Uses `http_client` for core downloads instead of creating a client.
    ///
    /// Pass the application's client to share its connections and settings
    /// with core.
    pub fn http_client(mut self, http_client: Arc<dyn HttpClient>) -> Self {
        self.http_client = Some(http_client);
        self
    }

    /// Starts `fvs2d` from `executable` when no daemon is listening.
    ///
    /// When unset, `fvs2d` is resolved through `PATH`. Relative paths are
    /// resolved against the process's current directory.
    #[cfg(feature = "fvs")]
    pub fn fvs2d(mut self, executable: impl Into<Option<std::path::PathBuf>>) -> Self {
        self.fvs2d = executable.into();
        self
    }

    /// Sets the remote runner, WineBridge, UMU, and prefix-component catalog.
    pub fn component_catalog(mut self, url: impl Into<Option<Url>>) -> Self {
        self.component_catalog = url.into();
        self
    }

    /// Sets the remote dependency catalog.
    pub fn dependency_catalog(mut self, url: impl Into<Option<Url>>) -> Self {
        self.dependency_catalog = url.into();
        self
    }

    /// Opens the application core and loads persisted state.
    ///
    /// This creates the application directories and loads cached addon
    /// catalogs, installed addons, and prefixes. With the `fvs` feature, it
    /// connects to or starts `fvs2d` before loading Virgo-backed prefixes.
    ///
    /// # Errors
    ///
    /// Returns an error if application directories cannot be created;
    /// persisted addons or prefixes cannot be read; the HTTP or download
    /// service cannot initialize; or an FVS executable cannot be resolved or
    /// contacted.
    pub async fn build(self) -> Result<Core> {
        paths::create().await?;
        let http_client = match self.http_client {
            Some(http_client) => http_client,
            None => Arc::new(ReqwestClient::new().map_err(download_manager::error::Error::from)?),
        };
        let context = Context::new(
            #[cfg(feature = "fvs")]
            crate::virgo::Fvs::connect(self.fvs2d).await?,
            http_client,
            self.component_catalog,
            self.dependency_catalog,
        )
        .await?;
        let prefixes = Prefixes::load(paths::prefixes(), context.clone()).await?;
        Ok(Core { context, prefixes })
    }
}
