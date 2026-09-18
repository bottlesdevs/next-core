//! Draft edits applied together by the owner's coordinated operation.

use crate::{Addon, Component, Dependency, EnvVars, Slot, Wrappers, error::Result};

use super::EnvironmentOwnerState;

/// A draft available only during an owner's edit callback.
/// Requirements are validated after the callback; identity and backend stay fixed.
pub struct Edit<'a, T> {
    pub(crate) draft: &'a mut T,
}

// The bound is internal: public handles construct edits for the supported states.
#[allow(private_bounds)]
impl<T: EnvironmentOwnerState> Edit<'_, T> {
    /// Select a frozen component without changing other slots. The same UUID is a no-op.
    pub fn set_component(&mut self, component: Addon<Component>) {
        self.draft.environment_mut().set_component(component);
    }

    /// Remove a selected component. An absent slot returns an error; requirements
    /// may be temporarily unsatisfied until the callback finishes.
    pub fn remove_component(&mut self, slot: Slot) -> Result<()> {
        self.draft.environment_mut().remove_component(slot)
    }

    /// Append a frozen dependency unless its UUID is already selected.
    /// Dependencies cannot be removed or reordered.
    pub fn add_dependency(&mut self, dependency: Addon<Dependency>) {
        self.draft.environment_mut().add_dependency(dependency);
    }

    /// Environment overrides used on the next startup.
    pub fn env_vars(&mut self) -> &mut EnvVars {
        &mut self.draft.environment_mut().env_vars
    }

    /// Command wrappers used on the next startup.
    pub fn wrappers(&mut self) -> &mut Wrappers {
        &mut self.draft.environment_mut().wrappers
    }
}
