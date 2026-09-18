//! Standalone Virgo programs with their own persisted environment and history.
mod manager;
pub use manager::ProgramManager;

use crate::{
    Edit, EnvironmentState, Operation, PrefixBackend, ProgramSpec, Snapshot, SnapshotSummary,
    environment::{Environment, EnvironmentOwnerState},
    error::Result,
    proto::{DllOverride, DllOverrideMode, Process},
};
use futures_core::Stream;
use next_config::Config;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

/// Complete persisted standalone state. Its launch definition supplies its name.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, Config)]
#[config(version = 1)]
pub struct ProgramState {
    pub(crate) id: Uuid,
    pub(crate) launch: ProgramSpec,
    pub(crate) environment: EnvironmentState,
}
impl ProgramState {
    pub fn id(&self) -> Uuid {
        self.id
    }
    pub fn name(&self) -> &str {
        self.launch.name()
    }
    pub fn launch(&self) -> &ProgramSpec {
        &self.launch
    }
    pub fn environment(&self) -> &EnvironmentState {
        &self.environment
    }
}
impl EnvironmentOwnerState for ProgramState {
    const FILE_NAME: &'static str = "program.toml";
    fn id(&self) -> Uuid {
        self.id
    }
    fn backend(&self) -> PrefixBackend {
        PrefixBackend::Virgo
    }
    fn environment(&self) -> &EnvironmentState {
        &self.environment
    }
    fn environment_mut(&mut self) -> &mut EnvironmentState {
        &mut self.environment
    }
    fn validate(&self) -> Result<()> {
        self.launch.validate()?;
        self.environment.validate()
    }
}

/// A live standalone program handle. Clones share state and coordination.
/// Dropping a handle does not stop Wine; state access fails after deletion.
#[derive(Clone)]
pub struct Program(pub(crate) Arc<Environment<ProgramState>>);
impl Program {
    pub fn id(&self) -> Result<Uuid> {
        Ok(self.state()?.id())
    }
    pub fn state(&self) -> Result<Arc<ProgramState>> {
        self.0.state()
    }
    pub fn watch(&self) -> impl Stream<Item = Arc<ProgramState>> + Send + 'static + use<> {
        self.0.watch()
    }

    /// Edit metadata, startup settings, and software selections in one draft.
    /// Validate the final state, apply software changes while stopped, then save
    /// and publish once. Metadata and startup settings can change while running.
    pub fn edit<R: Send + 'static>(
        &self,
        callback: impl FnOnce(&mut Edit<'_, ProgramState>) -> Result<R> + Send + 'static,
    ) -> Operation<R> {
        self.0.edit(callback)
    }
    /// Mount prepared Virgo storage and launch using this program's UUID as its group.
    pub fn launch(&self) -> Operation<u32> {
        self.0.launch(|state| Ok((state.id, state.launch.clone())))
    }
    pub async fn kill(&self) -> Result<()> {
        self.0.kill(|state| Ok(state.id)).await
    }
    pub async fn stop(&self) -> Result<()> {
        self.0.stop().await
    }
    pub async fn processes(&self) -> Result<Vec<Process>> {
        self.0.processes().await
    }
    pub fn dll_overrides(&self) -> Operation<Vec<DllOverride>> {
        self.0.dll_overrides()
    }
    pub fn set_dll_override(&self, dll: impl Into<String>, mode: DllOverrideMode) -> Operation<()> {
        self.0.set_dll_override(dll.into(), mode)
    }
    pub fn unset_dll_override(&self, dll: impl Into<String>) -> Operation<()> {
        self.0.unset_dll_override(dll.into())
    }
    /// Capture `program.toml` and persistent owner files while stopped and unmounted.
    /// Shared artifacts and external files are excluded; restoration does not rebuild layers.
    pub fn create_snapshot(&self, message: impl Into<String>) -> Operation<Snapshot> {
        self.0.create_snapshot(message.into())
    }
    pub async fn snapshots(&self) -> Result<Vec<SnapshotSummary>> {
        self.0.snapshots().await
    }
    /// Restore the managed root, validate it, then publish its ProgramState.
    /// Failed restoration recovers a checkpoint before returning. Explicit cancellation
    /// waits for active restoration and recovery to finish under coordination.
    pub fn rollback(&self, revision: &str) -> Operation<String> {
        self.0.rollback(revision)
    }
}
impl Edit<'_, ProgramState> {
    pub fn rename(&mut self, name: impl Into<String>) {
        self.draft.launch.rename(name);
    }
    pub fn launch(&mut self) -> &mut ProgramSpec {
        &mut self.draft.launch
    }
}
