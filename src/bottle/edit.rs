//! Controlled bottle metadata and settings edits.
use super::{Bottle, BottleState};
use crate::{Edit, Operation, error::Result};

impl Bottle {
    /// Edit a private draft, save it and publish it atomically. The callback's
    /// result is returned after saving. Settings apply on the next startup;
    /// software changes use explicit component and dependency operations.
    pub fn edit<R: Send + 'static>(
        &self,
        callback: impl FnOnce(&mut Edit<'_, BottleState>) -> Result<R> + Send + 'static,
    ) -> Operation<R> {
        self.0.edit(callback)
    }
}
