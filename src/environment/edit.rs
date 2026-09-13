//! Controlled edits of an owner's metadata and safe execution settings.

use crate::{BottleState, EnvVars, EnvironmentState, LaunchSpec, Wrappers};
use uuid::Uuid;

pub(crate) trait EnvironmentOwnerState {
    fn environment_mut(&mut self) -> &mut EnvironmentState;
}

/// A draft available only during an owner's edit callback.
/// Software selections, identity and backend are changed through owner operations.
pub struct Edit<'a, T> {
    pub(crate) draft: &'a mut T,
}

// The bound is internal: public handles construct edits for the supported states.
#[allow(private_bounds)]
impl<T: EnvironmentOwnerState> Edit<'_, T> {
    /// Environment overrides used on the next startup.
    pub fn env_vars(&mut self) -> &mut EnvVars {
        &mut self.draft.environment_mut().env_vars
    }

    /// Command wrappers used on the next startup.
    pub fn wrappers(&mut self) -> &mut Wrappers {
        &mut self.draft.environment_mut().wrappers
    }
}

impl Edit<'_, BottleState> {
    pub fn rename(&mut self, name: impl Into<String>) {
        self.draft.name = name.into();
    }

    pub fn add_program(&mut self, launch: LaunchSpec) -> Uuid {
        let id = Uuid::new_v4();
        self.draft.programs.insert(id, launch);
        id
    }

    pub fn remove_program(&mut self, id: Uuid) -> Option<LaunchSpec> {
        self.draft.programs.remove(&id)
    }

    /// Edit an existing launch definition without changing its registration ID.
    pub fn program(&mut self, id: Uuid) -> Option<&mut LaunchSpec> {
        self.draft.programs.get_mut(&id)
    }
}
