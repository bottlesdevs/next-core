//! Persisted bottle state and the shared bottle handle.

#[cfg(feature = "fvs")]
use crate::environment::VirgoManager;

use std::{
    collections::HashMap,
    hash::{Hash, Hasher},
    sync::Arc,
};

#[cfg(feature = "fvs")]
use std::path::PathBuf;

use futures_core::Stream;
use next_config::Config;
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, watch};
use tokio_stream::{StreamExt, wrappers::WatchStream};
use uuid::Uuid;

use super::error::BottleError;
use crate::{Context, EnvironmentState, PrefixBackend, addons::Addons, error::Result};

/// An immutable snapshot of a bottle's published configuration.
///
/// Snapshots are returned by [`Bottle::state`]  [`Bottle::watch`].
/// They remain valid after the bottle changes or is deleted;
/// their getters continue to return the values recorded when that particular
/// snapshot was published. Obtain another snapshot to observe later changes.
/// Component payload locations are derived from their UUIDs.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, Config)]
#[config(version = 2)]
pub struct BottleState {
    pub(crate) id: Uuid,
    pub(crate) name: String,
    pub(crate) backend: PrefixBackend,
    pub(crate) environment: EnvironmentState,
    #[serde(default)]
    pub(crate) programs: HashMap<Uuid, LaunchSpec>,
}

impl crate::environment::EnvironmentOwnerState for BottleState {
    fn environment_mut(&mut self) -> &mut EnvironmentState {
        &mut self.environment
    }
}

impl BottleState {
    pub(crate) fn validate(&self) -> Result<()> {
        self.environment.validate_requirements()?;
        for launch in self.programs.values() {
            launch.validate()?;
        }
        Ok(())
    }

    /// Returns the bottle's stable identity.
    pub fn id(&self) -> Uuid {
        self.id
    }

    /// Returns the display name.
    ///
    /// Names are not identities and need not be unique.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the execution settings shared by every registration in this bottle.
    pub fn environment(&self) -> &EnvironmentState {
        &self.environment
    }

    /// Iterates over registered programs in unspecified order.
    pub fn programs(&self) -> impl Iterator<Item = (Uuid, &LaunchSpec)> {
        self.programs.iter().map(|(id, launch)| (*id, launch))
    }

    /// Returns the backend fixed when this bottle was created.
    pub fn backend(&self) -> &PrefixBackend {
        &self.backend
    }

    /// Returns the registered program with identity `id`.
    pub fn program(&self, id: Uuid) -> Option<&LaunchSpec> {
        self.programs.get(&id)
    }
}

/// The shared coordination state behind cloned [`Bottle`] handles.
pub(crate) struct BottleInner {
    /// Latest state; `None` is the tombstone published when the bottle is deleted.
    pub(crate) published: watch::Sender<Option<Arc<BottleState>>>,
    /// Serializes control operations across cloned handles.
    pub(crate) control: Mutex<()>,
    /// Retained after deletion so stale handles report which bottle was deleted.
    pub(crate) id: Uuid,
    /// Shared services and storage locations scoped to the owning manager.
    pub(crate) cx: Context,
    /// Shared addon registry scoped to the owning manager.
    pub(crate) addons: Addons,
    #[cfg(feature = "fvs")]
    pub(crate) virgo: Arc<VirgoManager>,
}

/// A live, shared handle to one bottle.
///
/// Clones refer to the same bottle and publish the same immutable state
/// snapshots. A bottle's UUID is its identity; its display name may change and
/// may be shared by other bottles.
/// Hashing identifies the shared live handle and remains stable across state
/// publications and deletion.
///
/// Runtime operations attach to WineBridge for each control call. Calls
/// serialize within this core instance; the lock is released after launch, not
/// when the guest process exits. Dropping handles does not stop Wine.
#[derive(Clone)]
pub struct Bottle(pub(crate) Arc<BottleInner>);

// Allows the live handle to key iced subscriptions directly: clones must hash
// alike, while state publications and deletion must not change its identity.
impl Hash for Bottle {
    fn hash<H: Hasher>(&self, state: &mut H) {
        Arc::as_ptr(&self.0).hash(state);
    }
}

impl Bottle {
    /// Creates and persists the initial state before the handle is published by
    /// the manager.
    pub(crate) async fn new(
        id: Uuid,
        name: String,
        backend: PrefixBackend,
        environment: EnvironmentState,
        context: Context,
        addons: Addons,
        #[cfg(feature = "fvs")] virgo: Arc<VirgoManager>,
    ) -> Result<Self> {
        let state = BottleState {
            id,
            name,
            backend,
            environment,
            programs: HashMap::new(),
        };
        let bottle = Self::from_state(
            state,
            context,
            addons,
            #[cfg(feature = "fvs")]
            virgo,
        )?;
        bottle.save().await?;
        Ok(bottle)
    }

    /// Reconstructs a live handle after validating its addon requirements.
    pub(crate) fn from_state(
        state: BottleState,
        cx: Context,
        addons: Addons,
        #[cfg(feature = "fvs")] virgo: Arc<VirgoManager>,
    ) -> Result<Self> {
        state.validate()?;
        let id = state.id;
        let (published, _) = watch::channel(Some(Arc::new(state)));
        Ok(Self(Arc::new(BottleInner {
            id,
            published,
            control: Mutex::new(()),
            cx,
            addons,
            #[cfg(feature = "fvs")]
            virgo,
        })))
    }

    /// Returns this bottle's stable identity, including after deletion.
    pub fn id(&self) -> Uuid {
        self.0.id
    }

    /// Returns the latest published state.
    ///
    /// The returned [`Arc`] is a stable snapshot: later edits replace the
    /// published state rather than modifying it in place.
    ///
    /// # Errors
    ///
    /// Returns [`BottleError::Deleted`] after the bottle has been deleted.
    pub fn state(&self) -> Result<Arc<BottleState>> {
        self.0
            .published
            .borrow()
            .clone()
            .ok_or_else(|| BottleError::Deleted(self.0.id).into())
    }

    /// Watches this bottle's published state.
    ///
    /// The stream first yields the current snapshot, then the latest snapshot
    /// after each observed change. Slow consumers may miss intermediate states.
    /// Equal states are not republished. The stream ends when the bottle is
    /// deleted or all live handles are dropped. Snapshots already yielded
    /// remain usable afterward.
    pub fn watch(&self) -> impl Stream<Item = Arc<BottleState>> + Send + 'static + use<> {
        WatchStream::new(self.0.published.subscribe())
            .take_while(Option::is_some)
            .filter_map(|state| state)
    }

    #[cfg(feature = "fvs")]
    pub(crate) fn ensure_exists(&self) -> Result<()> {
        if self.is_deleted() {
            Err(BottleError::Deleted(self.0.id).into())
        } else {
            Ok(())
        }
    }

    #[cfg(feature = "fvs")]
    pub(crate) fn is_deleted(&self) -> bool {
        self.0.published.borrow().is_none()
    }

    /// Publishes the deletion tombstone, ending state streams without
    /// invalidating snapshots that callers already hold.
    pub(crate) fn mark_deleted(&self) {
        self.0.published.send_replace(None);
    }

    /// Publishes only observable state changes; an equal state does not wake
    /// watchers.
    pub(crate) fn publish(&self, state: BottleState) {
        let next = Arc::new(state);
        self.0.published.send_if_modified(|published| {
            if published.as_deref() == Some(next.as_ref()) {
                false
            } else {
                *published = Some(next);
                true
            }
        });
    }

    #[cfg(feature = "fvs")]
    pub(crate) fn bottle_path(&self) -> PathBuf {
        self.0.cx.directories().bottle(self.0.id)
    }

    async fn save(&self) -> Result<()> {
        let state = self.state()?;
        Self::save_state(&state, &self.0.cx).await
    }

    pub(super) async fn save_state(state: &BottleState, cx: &Context) -> Result<()> {
        let path = cx.directories().bottle(state.id).join("bottle.toml");
        next_config::save(path, state).await?;
        Ok(())
    }
}

/// A persisted, immutable Windows launch definition registered with a bottle.
#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LaunchSpec {
    name: String,
    executable: String,
    /// Command-line fragments joined with spaces before launch.
    ///
    /// Callers must include any Windows quoting needed for spaces or special
    /// characters inside an argument. An empty entry adds only a separator and
    /// does not produce an empty Windows argument.
    #[serde(default)]
    args: Vec<String>,
    /// Windows working directory, or WineBridge's inherited directory when absent.
    #[serde(default)]
    working_directory: Option<String>,
    /// Passed to WineBridge's `CREATE_NEW_CONSOLE` launch option.
    #[serde(default)]
    new_console: bool,
}

impl LaunchSpec {
    pub(crate) fn validate(&self) -> Result<()> {
        if self.name.trim().is_empty() {
            return Err(BottleError::InvalidProgram("name must not be blank".into()).into());
        }
        if self.executable.trim().is_empty() {
            return Err(BottleError::InvalidProgram("executable must not be blank".into()).into());
        }
        if self
            .working_directory
            .as_ref()
            .is_some_and(|path| path.trim().is_empty())
        {
            return Err(
                BottleError::InvalidProgram("working directory must not be blank".into()).into(),
            );
        }
        Ok(())
    }

    /// Creates a launch definition with default launch options.
    pub fn new(name: impl Into<String>, executable: impl Into<String>) -> Result<Self> {
        let name = name.into();
        let executable = executable.into();
        let program = Self {
            name,
            executable,
            args: Vec::new(),
            working_directory: None,
            new_console: false,
        };
        program.validate()?;
        Ok(program)
    }

    /// Replaces the Windows command-line fragments passed at launch.
    pub fn with_args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.args = args.into_iter().map(Into::into).collect::<Vec<_>>();
        self
    }

    /// Sets the Windows working directory used at launch.
    pub fn with_working_directory(mut self, working_directory: impl Into<String>) -> Result<Self> {
        let working_directory = working_directory.into();
        if working_directory.trim().is_empty() {
            return Err(
                BottleError::InvalidProgram("working directory must not be blank".into()).into(),
            );
        }
        self.working_directory = Some(working_directory);
        Ok(self)
    }

    /// Controls WineBridge's `CREATE_NEW_CONSOLE` launch option.
    pub fn with_new_console(mut self, new_console: bool) -> Self {
        self.new_console = new_console;
        self
    }

    /// Changes the display name; the owner validates it when saving an edit.
    pub fn rename(&mut self, name: impl Into<String>) {
        self.name = name.into();
    }

    /// Returns the display name preserved at registration.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the Windows executable path passed to WineBridge.
    pub fn executable(&self) -> &str {
        &self.executable
    }

    /// Returns the command-line fragments passed at launch.
    pub fn args(&self) -> &[String] {
        &self.args
    }

    /// Returns the configured Windows working directory, when present.
    pub fn working_directory(&self) -> Option<&str> {
        self.working_directory.as_deref()
    }

    /// Reports whether launch requests create a new console.
    pub fn new_console(&self) -> bool {
        self.new_console
    }
}
