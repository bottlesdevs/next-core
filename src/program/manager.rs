//! Registry-backed standalone program lifecycle.
use super::Program;
use crate::{
    Addon, Component, LibraryEntry, LibraryProvider, Manager, Operation, ProgramSpec,
    error::{Error, Result},
};
use uuid::Uuid;

#[async_trait::async_trait]
impl LibraryProvider for Manager<Program> {
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
impl Manager<Program> {
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
        self.create_environment(launch, runner, winebridge, umu)
    }
}
