//! Transactional edits to an environment configuration.
//!
//! An owner exposes [`Edit`] only inside its edit callback. Changes are made to
//! a private draft, validated as a whole, applied to storage, persisted, and
//! published only after the callback succeeds.

use crate::{Addon, Component, Dependency, EnvVars, Slot, Wrappers, error::Result};

#[cfg(feature = "fvs")]
use super::history;
use super::{BackendSource, Environment, State, backend::standard};
use crate::{
    EnvironmentError, Operation, PrefixBackend, Progress, Stage, error::Error,
    winebridge::WineBridgeClient,
};
use std::sync::Arc;

/// Provides mutable access to an environment's editable configuration.
///
/// Changes remain private until the owner operation completes. The environment
/// identifier, owner data, and [`PrefixBackend`] cannot be changed through this API.
pub struct Edit<'a, T> {
    pub(crate) draft: &'a mut State<T>,
}

impl<T> Edit<'_, T> {
    /// Selects `component` for the slot declared by that component.
    ///
    /// Other slots are unchanged. Selecting the same addon identifier preserves
    /// the frozen record already stored in the draft.
    pub fn set_component(&mut self, component: Addon<Component>) {
        self.draft.config.set_component(component);
    }

    /// Removes the component selected in `slot`.
    ///
    /// Requirement validation is deferred until the edit callback returns, so a
    /// caller may remove and replace a required component in one edit.
    ///
    /// # Errors
    ///
    /// Returns [`EnvironmentError::ComponentNotInstalled`] if `slot` is empty.
    pub fn remove_component(&mut self, slot: Slot) -> Result<()> {
        self.draft.config.remove_component(slot)
    }

    /// Appends `dependency` in installation order if it is not already selected.
    ///
    /// Duplicate addon identifiers are ignored. Dependencies cannot be removed
    /// or reordered through the edit API.
    pub fn add_dependency(&mut self, dependency: Addon<Dependency>) {
        self.draft.config.add_dependency(dependency);
    }

    /// Returns owner-level environment variables used on the next startup.
    ///
    /// These values override variables contributed by components and dependencies.
    pub fn env_vars(&mut self) -> &mut EnvVars {
        &mut self.draft.config.env_vars
    }

    /// Returns host command wrappers used on the next startup.
    pub fn wrappers(&mut self) -> &mut Wrappers {
        &mut self.draft.config.wrappers
    }
}

impl<T: BackendSource> Environment<T>
where
    State<T>: next_config::Config + Clone + PartialEq + Send + Sync,
{
    /// Runs a coordinated edit and publishes it only after application succeeds.
    ///
    /// Software changes require a stopped environment. Standard prefixes are
    /// changed in place; Virgo environments checkpoint and recover the owner if
    /// workspace preparation or persistence fails.
    ///
    /// # Errors
    ///
    /// Returns an error when the callback rejects the draft, validation fails,
    /// the environment is running during a software change, cancellation arrives
    /// before application is committed, or backend application/persistence fails.
    pub(crate) fn edit<R: Send + 'static>(
        self: &Arc<Self>,
        callback: impl FnOnce(&mut Edit<'_, T>) -> Result<R> + Send + 'static,
    ) -> Operation<R> {
        let environment = self.clone();
        Operation::new(move |progress, cancellation| async move {
            let _control = environment.lock_control(&cancellation).await?;
            let previous = environment.state()?;
            let mut draft = previous.as_ref().clone();
            let result = callback(&mut Edit { draft: &mut draft })?;
            draft.config.validate()?;
            if cancellation.is_cancelled() {
                return Err(Error::Cancelled);
            }
            if draft == *previous {
                return Ok(result);
            }
            let before = &previous.config;
            let after = &draft.config;
            let software_changed =
                before.components != after.components || before.dependencies != after.dependencies;
            if software_changed {
                if WineBridgeClient::try_connect(&environment.root.join("prefix"))
                    .await?
                    .is_some()
                {
                    return Err(EnvironmentError::MustBeStopped.into());
                }
                progress.send_replace(Some(Progress::new(Stage::Stopping)));
                environment.stop_locked().await?;
                if cancellation.is_cancelled() {
                    return Err(Error::Cancelled);
                }
            }
            // Once application succeeds, finish saving even if cancellation arrives.
            match (previous.data.backend(), software_changed) {
                (_, false) => environment.save(&draft).await?,
                (PrefixBackend::Standard, true) => {
                    standard::apply(
                        before,
                        after,
                        &environment.root,
                        &environment.context,
                        &progress,
                        &cancellation,
                    )
                    .await?;
                    environment.save(&draft).await?;
                }
                #[cfg(feature = "fvs")]
                (PrefixBackend::Virgo, true) => {
                    let (base, overlays) = environment
                        .virgo
                        .prepare_artifacts(after, &progress, &cancellation)
                        .await?;
                    let checkpoint = history::capture(
                        &environment.root,
                        history::AUTO_CHECKPOINT_MESSAGE.into(),
                        false,
                        Stage::Checkpointing,
                        &environment.context,
                        &progress,
                    )
                    .await?;
                    let applied = async {
                        environment
                            .virgo
                            .layers
                            .prepare_workspace(&environment.root, &base, &overlays, &cancellation)
                            .await?;
                        environment.save(&draft).await
                    }
                    .await;
                    history::recover(
                        applied,
                        &environment.root,
                        &checkpoint,
                        &environment.context,
                        &progress,
                    )
                    .await?;
                }
            }
            environment.publish(draft);
            Ok(result)
        })
    }
}
