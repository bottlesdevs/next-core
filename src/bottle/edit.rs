//! Coordinated edits to the latest persisted bottle configuration.

use super::{Bottle, BottleError, BottleState};
use crate::{
    Operation, Progress, Stage,
    error::{Error, Result},
};

impl Bottle {
    /// Applies a callback to a draft of the latest state when this operation runs.
    ///
    /// Edit `name`, `programs`, and `environment` directly. Returning an error
    /// discards the draft. Valid changes are reconciled, persisted, then published
    /// together; cloned handles serialize edits against the latest state.
    ///
    /// Metadata can change while running. Environment changes require an explicit
    /// stop first. Storage is fixed; Standard dependencies may only be appended.
    /// Addon selections must be downloaded.
    /// Standard mutations write directly; failed recipes can leave partial effects.
    /// Virgo prefix mutations checkpoint data and configuration together and restore both
    /// on failure. Settings-only edits use atomic configuration saving.
    pub fn edit(
        &self,
        callback: impl FnOnce(&mut BottleState) -> Result<()> + Send + 'static,
    ) -> Operation<()> {
        let bottle = self.clone();
        Operation::new(move |progress, cancellation| async move {
            progress.send_replace(Some(Progress::new(Stage::Preparing)));
            let cx = &bottle.0.cx;
            let _control = cancellation
                .run_until_cancelled(bottle.0.control.lock())
                .await
                .ok_or(Error::Cancelled)?;
            if cancellation.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let previous = bottle.state()?;
            let mut draft = previous.as_ref().clone();
            callback(&mut draft)?;
            if draft.id != bottle.id() {
                return Err(BottleError::IdMismatch {
                    expected: bottle.id(),
                    actual: draft.id,
                }
                .into());
            }
            for (id, program) in &draft.programs {
                if *id != program.id() {
                    return Err(BottleError::InvalidProgram(
                        "registration key must match the program ID".into(),
                    )
                    .into());
                }
                program.validate()?;
            }
            draft.environment.validate_requirements()?;
            let prefix_changed = crate::environment::validate_edit(
                &previous.environment,
                &draft.environment,
                &bottle.0.addons,
            )?;
            if cancellation.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let root = cx.directories().bottle(draft.id);
            let environment_changed = draft.environment != previous.environment;
            if environment_changed {
                if crate::environment::Environment::try_attach(&root)
                    .await?
                    .is_some()
                {
                    return Err(crate::EnvironmentError::MustBeStopped.into());
                }
                crate::environment::Environment::stop(&previous.environment, &root, cx).await?;
            }
            #[cfg(feature = "fvs")]
            let checkpoint = if prefix_changed
                && matches!(previous.environment.storage, crate::Storage::Virgo { .. })
            {
                Some(
                    crate::environment::history::capture(
                        &root,
                        crate::environment::history::AUTO_CHECKPOINT_MESSAGE.into(),
                        Stage::Checkpointing,
                        cx,
                        &progress,
                    )
                    .await?,
                )
            } else {
                None
            };
            let result = async {
                if prefix_changed {
                    crate::environment::reconcile(
                        &previous.environment,
                        &mut draft.environment,
                        &cx.directories().bottle(draft.id),
                        cx,
                        &bottle.0.addons,
                        &progress,
                        &cancellation,
                    )
                    .await?;
                }
                if cancellation.is_cancelled() {
                    return Err(Error::Cancelled);
                }
                Self::save_state(&draft, cx).await
            }
            .await;
            #[cfg(feature = "fvs")]
            let result = if let Some(checkpoint) = checkpoint {
                crate::environment::history::recover(result, &root, &checkpoint, cx, &progress)
                    .await
            } else {
                result
            };
            result?;
            bottle.publish(draft);
            Ok(())
        })
    }
}
