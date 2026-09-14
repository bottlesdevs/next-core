//! Shared ownership, publication and persistence for environment-backed handles.

#[cfg(feature = "fvs")]
use super::VirgoManager;
use super::prefix::standard;
use crate::{
    Addons, Context, EnvironmentError, EnvironmentState, PrefixBackend, Progress, Stage,
    error::{Error, Result},
};
use futures_core::Stream;
use next_config::Config;
use std::{path::PathBuf, sync::Arc};
use tokio::sync::{Mutex, MutexGuard, watch};
use tokio_stream::{StreamExt, wrappers::WatchStream};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

pub(crate) trait EnvironmentOwnerState: Config + Clone + PartialEq + Send + Sync {
    const FILE_NAME: &'static str;
    fn id(&self) -> Uuid;
    fn backend(&self) -> PrefixBackend;
    fn environment(&self) -> &EnvironmentState;
    fn environment_mut(&mut self) -> &mut EnvironmentState;
    fn validate(&self) -> Result<()>;
}

pub(crate) struct Environment<T> {
    pub(crate) published: watch::Sender<Option<Arc<T>>>,
    pub(crate) control: Mutex<()>,
    pub(crate) root: PathBuf,
    pub(crate) context: Context,
    pub(crate) addons: Addons,
    #[cfg(feature = "fvs")]
    pub(crate) virgo: Arc<VirgoManager>,
}

impl<T: EnvironmentOwnerState> Environment<T> {
    pub(crate) fn from_state(
        state: T,
        root: PathBuf,
        context: Context,
        addons: Addons,
        #[cfg(feature = "fvs")] virgo: Arc<VirgoManager>,
    ) -> Result<Arc<Self>> {
        state.validate()?;
        let expected = root
            .file_name()
            .and_then(|s| s.to_str())
            .and_then(|s| s.parse::<Uuid>().ok())
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "environment directory must be a UUID",
                )
            })?;
        if state.id() != expected {
            return Err(EnvironmentError::IdMismatch {
                expected,
                actual: state.id(),
            }
            .into());
        }
        let (published, _) = watch::channel(Some(Arc::new(state)));
        Ok(Arc::new(Self {
            published,
            control: Mutex::new(()),
            root,
            context,
            addons,
            #[cfg(feature = "fvs")]
            virgo,
        }))
    }

    /// Creation is private until initialization and persistence both succeed.
    pub(crate) async fn create(
        state: T,
        root: PathBuf,
        context: Context,
        addons: Addons,
        #[cfg(feature = "fvs")] virgo: Arc<VirgoManager>,
        progress: &watch::Sender<Option<Progress>>,
        cancellation: &CancellationToken,
    ) -> Result<Arc<Self>> {
        let environment = Self::from_state(
            state,
            root,
            context,
            addons,
            #[cfg(feature = "fvs")]
            virgo,
        )?;
        let state = environment.state()?;
        if cancellation.is_cancelled() {
            return Err(Error::Cancelled);
        }
        progress.send_replace(Some(Progress::new(Stage::CreatingPrefix)));
        // Initialization owns shutdown. Retain storage when it cannot finish safely.
        match state.backend() {
            PrefixBackend::Standard => {
                standard::create(state.environment(), &environment.root, &environment.context)
                    .await?;
            }
            #[cfg(feature = "fvs")]
            PrefixBackend::Virgo => {
                environment
                    .virgo
                    .apply(state.environment(), progress, cancellation)
                    .await?;
                async_fs::create_dir_all(environment.root.join("upper")).await?;
            }
        }
        let result = async {
            if cancellation.is_cancelled() {
                return Err(Error::Cancelled);
            }
            progress.send_replace(Some(Progress::new(Stage::Configuring)));
            environment.save(&state).await?;
            if cancellation.is_cancelled() {
                return Err(Error::Cancelled);
            }
            Ok(())
        }
        .await;
        if result.is_err() {
            let _ = async_fs::remove_dir_all(&environment.root).await;
        }
        result?;
        Ok(environment)
    }

    pub(crate) fn state(&self) -> Result<Arc<T>> {
        self.published
            .borrow()
            .clone()
            .ok_or_else(|| EnvironmentError::Deleted.into())
    }

    pub(crate) fn watch(&self) -> impl Stream<Item = Arc<T>> + Send + 'static + use<T> {
        WatchStream::new(self.published.subscribe())
            .take_while(Option::is_some)
            .filter_map(|state| state)
    }

    pub(crate) fn publish(&self, state: T) {
        let next = Arc::new(state);
        self.published.send_if_modified(|published| {
            if published.as_deref() == Some(next.as_ref()) {
                return false;
            }
            *published = Some(next);
            true
        });
    }

    pub(crate) async fn save(&self, state: &T) -> Result<()> {
        Ok(next_config::save(self.root.join(T::FILE_NAME), state).await?)
    }

    pub(crate) async fn lock_control(
        &self,
        cancellation: &CancellationToken,
    ) -> Result<MutexGuard<'_, ()>> {
        let guard = cancellation
            .run_until_cancelled(self.control.lock())
            .await
            .ok_or(Error::Cancelled)?;
        if cancellation.is_cancelled() {
            return Err(Error::Cancelled);
        }
        Ok(guard)
    }

    pub(crate) async fn delete(
        &self,
        progress: &watch::Sender<Option<Progress>>,
        cancellation: &CancellationToken,
    ) -> Result<()> {
        let _control = self.lock_control(cancellation).await?;
        progress.send_replace(Some(Progress::new(Stage::Stopping)));
        self.stop_locked().await?;
        if cancellation.is_cancelled() {
            return Err(Error::Cancelled);
        }
        progress.send_replace(Some(Progress::new(Stage::Removing)));
        async_fs::remove_dir_all(&self.root).await?;
        self.published.send_replace(None);
        Ok(())
    }
}
