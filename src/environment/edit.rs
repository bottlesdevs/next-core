//! Controlled edits of an owner's metadata and safe execution settings.

use crate::{EnvVars, Wrappers};

use super::EnvironmentOwnerState;

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
