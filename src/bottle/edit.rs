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
    /// stop first. Storage and existing dependency order cannot be changed; new
    /// dependencies may be appended. Addon selections must be downloaded.
    /// Prefix effects are not yet rolled back as a batch if reconciliation or
    /// persistence fails; no candidate configuration is published on failure.
    pub fn edit(
        &self,
        callback: impl FnOnce(&mut BottleState) -> Result<()> + Send + 'static,
    ) -> Operation<()> {
        let bottle = self.clone();
        Operation::new(move |progress, cancellation| async move {
            progress.send_replace(Some(Progress::new(Stage::Preparing)));
            bottle
                .update(Some(&cancellation), async |draft, cx, cached| {
                    let previous = draft.environment.clone();
                    callback(draft)?;
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
                    if cancellation.is_cancelled() {
                        return Err(Error::Cancelled);
                    }
                    if draft.environment != previous {
                        let environment = cached.get_or_insert_with(|| {
                            crate::environment::Environment::new(
                                previous,
                                cx.directories().bottle(draft.id),
                                cx,
                            )
                        });
                        environment.ensure_stopped().await?;
                        if cancellation.is_cancelled() {
                            return Err(Error::Cancelled);
                        }
                        // Failed reconciliation must not retain its candidate configuration.
                        let mut environment = cached.take().expect("environment initialized");
                        environment
                            .reconcile(
                                draft.environment.clone(),
                                &bottle.0.addons,
                                &progress,
                                &cancellation,
                            )
                            .await?;
                        if cancellation.is_cancelled() {
                            return Err(Error::Cancelled);
                        }
                        draft.environment = environment.config.clone();
                        *cached = Some(environment);
                    }
                    Ok(())
                })
                .await
        })
    }
}
