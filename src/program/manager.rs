//! Registry-backed standalone program lifecycle.
use super::{Program, ProgramState};
use crate::{
    Addons, Context, EnvironmentState, Operation, ProgramSpec,
    environment::{Environment, Registry, VirgoManager},
    error::Result,
};
use futures_core::Stream;
use futures_util::StreamExt;
use std::{
    hash::{Hash, Hasher},
    sync::Arc,
};
use uuid::Uuid;

#[derive(Clone)]
pub struct ProgramManager {
    context: Context,
    addons: Addons,
    virgo: Arc<VirgoManager>,
    registry: Arc<Registry<ProgramState>>,
}
impl Hash for ProgramManager {
    fn hash<H: Hasher>(&self, state: &mut H) {
        Arc::as_ptr(&self.registry).hash(state);
    }
}
impl ProgramManager {
    pub(crate) async fn load(
        context: Context,
        addons: Addons,
        virgo: Arc<VirgoManager>,
    ) -> Result<Self> {
        let registry = Arc::new(
            Registry::load(&context.directories().programs(), &context, &addons, &virgo).await?,
        );
        Ok(Self {
            context,
            addons,
            virgo,
            registry,
        })
    }

    /// Create private Virgo storage and save the launch definition. Requires downloaded
    /// runtime releases; does not acquire the application or prepare shared layers.
    pub fn create(&self, launch: ProgramSpec, runner: Uuid) -> Operation<Program> {
        let manager = self.clone();
        Operation::new(move |progress, cancellation| async move {
            let id = Uuid::new_v4();
            let state = ProgramState {
                id,
                launch,
                environment: EnvironmentState::new(runner, &manager.addons)?,
            };
            let environment = Environment::create(
                state,
                manager.context.directories().program(id),
                manager.context,
                manager.addons,
                manager.virgo,
                &progress,
                &cancellation,
            )
            .await?;
            Ok(Program(manager.registry.intern(id, environment)))
        })
    }
    /// Open an already-known program without filesystem or runtime work.
    pub async fn open(&self, id: Uuid) -> Result<Program> {
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
    /// Stop the environment and remove its managed root. Existing handles become deleted.
    pub fn delete(&self, id: Uuid) -> Operation<()> {
        let manager = self.clone();
        Operation::new(move |progress, cancellation| async move {
            let program = manager.open(id).await?;
            program.0.delete(&progress, &cancellation).await?;
            manager.registry.remove(id);
            Ok(())
        })
    }
}
