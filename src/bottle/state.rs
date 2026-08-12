//! Persisted bottle state and the shared bottle handle.

use std::{collections::HashMap, ops::AsyncFnOnce, path::PathBuf, sync::Arc};

use futures_core::Stream;
use next_config::Config;
use serde::{Deserialize, Serialize};
use tokio::sync::{RwLock, watch};
use tokio_stream::{StreamExt, wrappers::WatchStream};
use uuid::Uuid;

use super::{edit::BottleEdit, error::BottleError};
use crate::{
    Context,
    addons::{Addon, Addons, Component, Dependency, Requirement, Slot},
    error::Result,
    prefix::Prefix,
    utils::environment::Environment,
    wrapper::Wrappers,
};

/// An immutable snapshot of a bottle's published configuration.
///
/// Snapshots are returned by [`Bottle::state`]  [`Bottle::watch`].
/// They remain valid after the bottle changes or is deleted;
/// their getters continue to return the values recorded when that particular
/// snapshot was published. Obtain another snapshot to observe later changes.
/// Component locations are derived from their slot and version.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, Config)]
#[config(version = 1)]
pub struct BottleState {
    pub(crate) id: Uuid,
    pub(crate) name: String,
    pub(crate) storage: Prefix,
    #[serde(default)]
    pub(crate) programs: Vec<Program>,

    /// Runtime and prefix components pinned to exact releases.
    pub(crate) components: HashMap<Slot, Addon<Component>>,
    /// Installed dependency releases.
    pub(crate) dependencies: Vec<Addon<Dependency>>,
    #[serde(default, skip_serializing_if = "Environment::is_empty")]
    pub(crate) environment: Environment,

    #[serde(flatten)]
    pub(crate) wrappers: Wrappers,
}

impl BottleState {
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

    /// Returns the runner recorded when this snapshot was published.
    ///
    /// Catalog refreshes do not replace this value.
    pub fn runner(&self) -> &Addon<Component> {
        self.component(Slot::Runner)
            .expect("persisted bottle state is runtime-validated")
    }

    /// Returns the exact WineBridge release selected for this bottle.
    pub fn winebridge(&self) -> &Addon<Component> {
        self.component(Slot::WineBridge)
            .expect("persisted bottle state is runtime-validated")
    }

    /// Returns the selected UMU release, if this runtime uses one.
    pub fn umu(&self) -> Option<&Addon<Component>> {
        self.component(Slot::Umu)
    }

    /// Returns exact component releases keyed by their occupied slots.
    pub fn components(&self) -> &HashMap<Slot, Addon<Component>> {
        &self.components
    }

    /// Returns every dependency installed in this bottle.
    pub fn dependencies(&self) -> &[Addon<Dependency>] {
        &self.dependencies
    }

    /// Returns the component occupying `slot`, if any.
    pub fn component(&self, slot: Slot) -> Option<&Addon<Component>> {
        self.components.get(&slot)
    }

    /// Returns the installed dependency with this release identifier.
    pub fn dependency(&self, id: Uuid) -> Option<&Addon<Dependency>> {
        self.dependencies
            .iter()
            .find(|dependency| dependency.id() == id)
    }

    pub(crate) fn contains_addon_matching(&self, requirement: &Requirement) -> bool {
        self.components
            .values()
            .any(|component| component.satisfies(requirement))
            || self
                .dependencies
                .iter()
                .any(|dependency| dependency.satisfies(requirement))
    }

    pub(crate) fn validate_requirements(&self) -> Result<()> {
        for (slot, component) in &self.components {
            if component.slot() != *slot {
                return Err(BottleError::InvalidComponentSlot {
                    component: component.id(),
                    required: *slot,
                }
                .into());
            }
        }

        let missing = [Slot::WineBridge, Slot::Runner]
            .into_iter()
            .filter(|slot| self.component(*slot).is_none())
            .map(Requirement::Slot)
            .collect::<Vec<_>>();
        if !missing.is_empty() {
            return Err(BottleError::RequiresAddon {
                required_by: None,
                requirements: missing,
            }
            .into());
        }

        for (id, requirements) in self
            .components
            .values()
            .map(|addon| (addon.id(), addon.requirements()))
            .chain(
                self.dependencies
                    .iter()
                    .map(|addon| (addon.id(), addon.requirements())),
            )
        {
            let missing = requirements
                .iter()
                .filter(|requirement| !self.contains_addon_matching(requirement))
                .cloned()
                .collect::<Vec<_>>();
            if !missing.is_empty() {
                return Err(BottleError::RequiresAddon {
                    required_by: Some(id),
                    requirements: missing,
                }
                .into());
            }
        }
        Ok(())
    }

    /// Returns environment variables supplied when WineBridge is started.
    ///
    /// Changes do not affect an already-running WineBridge. Call
    /// [`Bottle::stop`] before the next bridge-backed operation to apply them
    /// immediately.
    pub fn environment(&self) -> &Environment {
        &self.environment
    }

    /// Returns the wrapper configuration applied when WineBridge is started.
    ///
    /// Changes do not affect an already-running WineBridge. Call
    /// [`Bottle::stop`] before the next bridge-backed operation to apply them
    /// immediately.
    pub fn wrappers(&self) -> &Wrappers {
        &self.wrappers
    }

    /// Preserves registration order and may contain duplicate UUIDs.
    pub fn programs(&self) -> &[Program] {
        &self.programs
    }

    /// Returns the first registered program with identity `id`.
    ///
    /// Duplicate UUIDs are currently accepted, so callers that construct
    /// [`Program`] values directly are responsible for keeping them unique.
    pub fn program(&self, id: Uuid) -> Option<&Program> {
        self.programs.iter().find(|program| program.id == id)
    }

    /// Reports how the Wine prefix itself is stored.
    ///
    /// Both strategies still use FVS for snapshot history and addon mutation
    /// checkpoints.
    pub fn storage(&self) -> Storage {
        self.storage.kind()
    }
}

/// The shared coordination state behind cloned [`Bottle`] handles.
pub(crate) struct BottleInner {
    /// Latest state; `None` is the tombstone published when the bottle is deleted.
    pub(crate) published: watch::Sender<Option<Arc<BottleState>>>,
    /// Excludes metadata and destructive operations while bridge calls hold shared access.
    pub(crate) write_lock: RwLock<()>,
    /// Retained after deletion so stale handles report which bottle was deleted.
    pub(crate) id: Uuid,
    /// Shared services and storage locations scoped to the owning manager.
    pub(crate) cx: Context,
    /// Shared addon registry scoped to the owning manager.
    pub(crate) addons: Addons,
}

/// A live, shared handle to one bottle.
///
/// Clones refer to the same bottle and publish the same immutable state
/// snapshots. A bottle's UUID is its identity; its display name may change and
/// may be shared by other bottles.
///
/// Methods that access WineBridge start it on demand. Once it is running,
/// requests may run concurrently. As a current limitation, callers must
/// serialize simultaneous first bridge-backed calls for a stopped bottle to
/// avoid racing two WineBridge starts.
#[derive(Clone)]
pub struct Bottle(pub(crate) Arc<BottleInner>);

impl Bottle {
    /// Creates and persists the initial state before the handle is published by
    /// the manager.
    pub(crate) async fn new(
        id: Uuid,
        name: String,
        components: HashMap<Slot, Addon<Component>>,
        dependencies: Vec<Addon<Dependency>>,
        storage: Prefix,
        context: Context,
        addons: Addons,
    ) -> Result<Self> {
        let state = BottleState {
            id,
            name,
            components,
            dependencies,
            storage,
            programs: Vec::new(),
            wrappers: Wrappers::default(),
            environment: Environment::default(),
        };
        let bottle = Self::from_state(state, context, addons)?;
        bottle.save().await?;
        Ok(bottle)
    }

    /// Reconstructs a live handle after validating its addon requirements.
    pub(crate) fn from_state(state: BottleState, cx: Context, addons: Addons) -> Result<Self> {
        state.validate_requirements()?;
        let id = state.id;
        let (published, _) = watch::channel(Some(Arc::new(state)));
        Ok(Self(Arc::new(BottleInner {
            id,
            published,
            write_lock: RwLock::new(()),
            cx,
            addons,
        })))
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
    pub fn watch(&self) -> impl Stream<Item = Arc<BottleState>> + Send + 'static {
        WatchStream::new(self.0.published.subscribe())
            .take_while(Option::is_some)
            .filter_map(|state| state)
    }

    /// Starts a batch of configuration changes.
    ///
    /// No changes are made until [`BottleEdit::commit`] is awaited.
    pub fn edit(&self) -> BottleEdit {
        BottleEdit::new(self.clone())
    }

    pub(crate) fn ensure_exists(&self) -> Result<()> {
        if self.is_deleted() {
            Err(BottleError::Deleted(self.0.id).into())
        } else {
            Ok(())
        }
    }

    pub(crate) fn is_deleted(&self) -> bool {
        self.0.published.borrow().is_none()
    }

    /// Publishes the deletion tombstone, ending state streams without
    /// invalidating snapshots that callers already hold.
    pub(crate) fn mark_deleted(&self) {
        self.0.published.send_replace(None);
    }

    /// Serializes a mutation against the latest state and publishes only after
    /// persistence succeeds.
    ///
    /// The operation may perform external prefix work before `save_state`; such
    /// side effects are not automatically reversed if persistence then fails.
    pub(super) async fn update<F, R>(&self, operation: F) -> Result<R>
    where
        F: for<'a> AsyncFnOnce(&'a mut BottleState, Context) -> Result<R>,
    {
        let _write = self.0.write_lock.write().await;
        let mut draft = self.state()?.as_ref().clone();
        let value = operation(&mut draft, self.0.cx.clone()).await?;
        Self::save_state(&draft, &self.0.cx).await?;
        self.publish(draft);
        Ok(value)
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

    pub(crate) fn bottle_path(&self) -> PathBuf {
        self.0.cx.directories().bottle(self.0.id)
    }

    pub(crate) fn prefix_path(&self) -> PathBuf {
        self.bottle_path().join("prefix")
    }

    async fn save(&self) -> Result<()> {
        let state = self.state()?;
        Self::save_state(&state, &self.0.cx).await
    }

    async fn save_state(state: &BottleState, cx: &Context) -> Result<()> {
        let path = cx.directories().bottle(state.id).join("bottle.toml");
        next_config::save(path, state).await?;
        Ok(())
    }
}

/// A persisted Windows launch definition registered with a bottle.
///
/// The UUID is the program's identity and is also used to group processes
/// launched by [`Bottle::run`]. UUID uniqueness is not enforced when programs
/// are added, and the public fields allow callers to replace a generated ID.
#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct Program {
    /// Identity used for lookup and process grouping.
    pub id: Uuid,
    /// Display name, preserved verbatim including surrounding whitespace.
    pub name: String,
    /// Windows path passed to `CreateProcessW` as the executable.
    pub executable: String,
    /// Command-line fragments joined with spaces before launch.
    ///
    /// Callers must include any Windows quoting needed for spaces or special
    /// characters inside an argument. An empty entry adds only a separator and
    /// does not produce an empty Windows argument.
    #[serde(default)]
    pub args: Vec<String>,
    /// Windows working directory, or WineBridge's inherited directory when absent.
    #[serde(default)]
    pub working_directory: Option<String>,
    /// Passed to WineBridge's `CREATE_NEW_CONSOLE` launch option.
    #[serde(default)]
    pub new_console: bool,
}

impl Program {
    /// Creates a program with a new UUID and default launch options.
    ///
    /// Arguments are empty, the working directory is inherited, and no new
    /// console is requested. The name and executable are stored without
    /// validation; [`BottleEdit::commit`] validates them when the program is
    /// registered.
    pub fn new(name: impl Into<String>, executable: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: name.into(),
            executable: executable.into(),
            args: Vec::new(),
            working_directory: None,
            new_console: false,
        }
    }
}

/// The prefix-storage strategy persisted in [`BottleState`].
#[derive(Debug, Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
pub enum Storage {
    /// Stores a conventional mutable prefix in the bottle directory.
    ///
    /// FVS is still required for bottle snapshots and addon mutation
    /// checkpoints.
    Standard,
    /// Stores the prefix as composable FVS layers.
    ///
    /// Virgo is experimental and requires the configured FVS service.
    Virgo,
}
