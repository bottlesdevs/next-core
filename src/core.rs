//! Initialization and shared services for a Bottles application.

use std::sync::Arc;

use http_client::{HttpClient, ReqwestClient};
use url::Url;

use crate::{Addons, Catalogs, Context, Prefixes, error::Result, utils::paths};

/// Owns the services and persisted collections used by Bottles Next.
///
/// Create one instance with [`Core::builder`], borrow its collections for the
/// lifetime of the application, then call [`Core::shutdown`] after all
/// outstanding [`Operation`](crate::Operation) values have completed.
pub struct Core {
    context: Context,
    catalogs: Catalogs,
    prefixes: Prefixes,
}

/// Startup options for a [`Core`], opened with [`CoreBuilder::build`].
///
/// Every option is optional. Catalog sources default to the official Bottles
/// catalogs and are used by [`Catalogs::refresh`]; cached catalogs and installed
/// addons still load without them.
#[must_use = "call `build` to open the core"]
#[derive(Default)]
pub struct CoreBuilder {
    http_client: Option<Arc<dyn HttpClient>>,
    #[cfg(feature = "fvs")]
    fvs2d: Option<std::path::PathBuf>,
    catalogs: Option<Vec<Url>>,
}

const DEFAULT_CATALOGS: [&str; 2] = [
    "https://bottles-next-deps.bromb.in/api/v1/catalog/components",
    "https://bottles-next-deps.bromb.in/api/v1/catalog/dependencies",
];

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
        self.catalogs.shutdown().await;
        Ok(())
    }

    /// Returns the collection of persisted [`Prefix`](crate::Prefix) handles.
    pub fn prefixes(&self) -> &Prefixes {
        &self.prefixes
    }

    /// Returns the store of locally acquired releases.
    pub fn addons(&self) -> &Addons {
        self.context.addons()
    }

    /// Returns the remote catalogs that acquire releases into [`Self::addons`].
    pub fn catalogs(&self) -> &Catalogs {
        &self.catalogs
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

    /// Replaces the default catalog sources.
    ///
    /// When several sources list the same release, the earliest wins.
    pub fn catalogs(mut self, urls: impl IntoIterator<Item = Url>) -> Self {
        self.catalogs = Some(urls.into_iter().collect());
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
        )
        .await?;
        let sources = self.catalogs.unwrap_or_else(|| {
            DEFAULT_CATALOGS
                .iter()
                .map(|url| Url::parse(url).expect("default catalog URL is valid"))
                .collect()
        });
        let catalogs = Catalogs::load(http_client, context.addons().clone(), sources).await?;
        let prefixes = Prefixes::load(paths::prefixes(), context.clone()).await?;
        Ok(Core {
            context,
            catalogs,
            prefixes,
        })
    }
}
