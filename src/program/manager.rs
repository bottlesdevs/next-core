//! Registry-backed standalone program lifecycle.
use super::{Program, ProgramState};
use crate::{
    Addon, Component, Context, EnvironmentState, Operation, ProgramSpec,
    environment::{Environment, Registry, VirgoManager},
    error::Result,
};
use futures_core::Stream;
use futures_util::StreamExt;
use std::sync::Arc;
use uuid::Uuid;

#[derive(Clone)]
pub struct ProgramManager {
    context: Context,
    virgo: Arc<VirgoManager>,
    registry: Arc<Registry<ProgramState>>,
}
impl ProgramManager {
    pub(crate) async fn load(context: Context, virgo: Arc<VirgoManager>) -> Result<Self> {
        let registry =
            Arc::new(Registry::load(&context.directories().programs(), &context, &virgo).await?);
        Ok(Self {
            context,
            virgo,
            registry,
        })
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
        let manager = self.clone();
        Operation::new(move |progress, cancellation| async move {
            let id = Uuid::new_v4();
            let state = ProgramState {
                id,
                launch,
                environment: EnvironmentState::new(runner, winebridge, umu)?,
            };
            let environment = Environment::create(
                state,
                manager.context.directories().program(id),
                manager.context,
                manager.virgo,
                &progress,
                &cancellation,
            )
            .await?;
            manager.registry.insert(environment.clone())?;
            Ok(Program(environment))
        })
    }
    /// Look up an already-known program synchronously, without filesystem or runtime work.
    pub fn open(&self, id: Uuid) -> Result<Program> {
        self.registry
            .get(id)
            .map(Program)
            .ok_or_else(|| crate::EnvironmentError::NotFound(id).into())
    }
    pub fn list(&self) -> Vec<Program> {
        self.registry.list().into_iter().map(Program).collect()
    }
    /// Observe membership and state changes; ends when the manager is dropped.
    pub fn watch(&self) -> impl Stream<Item = Vec<Program>> + Send + 'static + use<> {
        self.registry
            .watch()
            .map(|environments| environments.into_iter().map(Program).collect())
    }
    /// Stop and withdraw the managed root into trash. Existing handles become deleted;
    /// cleanup is best effort after withdrawal.
    pub fn delete(&self, id: Uuid) -> Operation<()> {
        let manager = self.clone();
        Operation::new(move |progress, cancellation| async move {
            let program = manager.open(id)?;
            program.0.delete(&progress, &cancellation).await?;
            manager.registry.remove(id);
            Ok(())
        })
    }
}
