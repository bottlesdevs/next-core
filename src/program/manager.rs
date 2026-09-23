//! Registry-backed standalone program lifecycle.
use super::Program;
use crate::{
    Addon, Component, Context, LibraryEntry, LibraryProvider, Operation, ProgramSpec,
    environment::{Manager, VirgoManager},
    error::{Error, Result},
};
use futures_core::Stream;
use futures_util::StreamExt;
use std::sync::Arc;
use uuid::Uuid;

#[derive(Clone)]
pub struct ProgramManager(Arc<Manager<ProgramSpec>>);

#[async_trait::async_trait]
impl LibraryProvider for ProgramManager {
    fn id(&self) -> &str {
        "programs"
    }

    async fn list_entries(&self) -> Result<Vec<LibraryEntry>> {
        Ok(self
            .list()
            .into_iter()
            .filter_map(|program| program.state().ok())
            .map(|state| LibraryEntry {
                id: state.id().to_string(),
                title: state.name().to_owned(),
            })
            .collect())
    }

    fn launch(&self, entry_id: &str) -> Result<Operation<()>> {
        let id = Uuid::parse_str(entry_id).map_err(|error| Error::LibraryProvider {
            provider: self.id().to_owned(),
            message: error.to_string(),
        })?;
        Ok(self.open(id)?.launch().map(|_| ()))
    }
}
impl ProgramManager {
    pub(crate) async fn load(context: Context, virgo: Arc<VirgoManager>) -> Result<Self> {
        Ok(Self(
            Manager::load(context.directories().programs(), context, virgo).await?,
        ))
    }

    /// Build missing Virgo layers, prepare the private registry, and save the launch definition.
    /// Requires downloaded runtime and build inputs; does not acquire the application.
    /// Callers supply every owner runtime selection, including UMU when required.
    pub fn create(
        &self,
        launch: ProgramSpec,
        runner: Addon<Component>,
        winebridge: Addon<Component>,
        umu: Option<Addon<Component>>,
    ) -> Operation<Program> {
        self.0.create(launch, runner, winebridge, umu).map(Program)
    }

    /// Look up an already-known program synchronously, without filesystem or runtime work.
    pub fn open(&self, id: Uuid) -> Result<Program> {
        self.0.open(id).map(Program)
    }

    pub fn list(&self) -> Vec<Program> {
        self.0.list().into_iter().map(Program).collect()
    }
    /// Observe membership and state changes; ends when the manager is dropped.
    pub fn watch(&self) -> impl Stream<Item = Vec<Program>> + Send + 'static + use<> {
        self.0
            .watch()
            .map(|environments| environments.into_iter().map(Program).collect())
    }
    /// Stop and withdraw the managed root into trash. Existing handles become deleted;
    /// cleanup is best effort after withdrawal.
    pub fn delete(&self, id: Uuid) -> Operation<()> {
        self.0.delete(id)
    }
}
