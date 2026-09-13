//! Coordinated metadata and execution-setting edits.

use super::{Bottle, BottleState};
use crate::{
    Edit, Operation,
    error::{Error, Result},
};

impl Bottle {
    /// Clone, edit, validate, save and publish the latest state under coordination.
    /// Returning an error discards the draft. Settings apply on the next startup;
    /// editing does not start Wine or run installers. Software changes use explicit
    /// component and dependency operations.
    pub fn edit<R: Send + 'static>(
        &self,
        callback: impl FnOnce(&mut Edit<'_, BottleState>) -> Result<R> + Send + 'static,
    ) -> Operation<R> {
        let bottle = self.clone();
        Operation::new(move |_, cancellation| async move {
            let _control = cancellation
                .run_until_cancelled(bottle.0.control.lock())
                .await
                .ok_or(Error::Cancelled)?;
            if cancellation.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let previous = bottle.state()?;
            let mut draft = previous.as_ref().clone();
            let result = callback(&mut Edit { draft: &mut draft })?;
            draft.validate()?;
            if cancellation.is_cancelled() {
                return Err(Error::Cancelled);
            }
            Self::save_state(&draft, &bottle.0.cx).await?;
            bottle.publish(draft);
            Ok(result)
        })
    }
}
