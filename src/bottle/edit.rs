//! Controlled bottle metadata and settings edits.
use super::{Bottle, BottleState};
use crate::{Edit, Operation, ProgramSpec, error::Result};
use uuid::Uuid;

impl Bottle {
    /// Edit a private draft, saving changes before publishing them. Unchanged
    /// drafts return the callback's result without saving or publishing.
    /// Settings apply on the next startup;
    /// software changes use explicit component and dependency operations.
    pub fn edit<R: Send + 'static>(
        &self,
        callback: impl FnOnce(&mut Edit<'_, BottleState>) -> Result<R> + Send + 'static,
    ) -> Operation<R> {
        self.0.edit(callback)
    }
}

impl Edit<'_, BottleState> {
    pub fn rename(&mut self, name: impl Into<String>) {
        self.draft.name = name.into();
    }

    pub fn add_program(&mut self, launch: ProgramSpec) -> Uuid {
        let id = Uuid::new_v4();
        self.draft.programs.insert(id, launch);
        id
    }

    pub fn remove_program(&mut self, id: Uuid) -> Option<ProgramSpec> {
        self.draft.programs.remove(&id)
    }

    /// Edit an existing launch definition without changing its registration ID.
    pub fn program(&mut self, id: Uuid) -> Option<&mut ProgramSpec> {
        self.draft.programs.get_mut(&id)
    }
}
