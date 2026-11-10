//! Coordinated edits to a prefix configuration.
//!
//! [`Edit`] is exposed only inside a prefix's edit callback. Changes are made to
//! a private draft, validated as a whole, applied to storage, persisted, and
//! published only after the callback succeeds. With the `fvs` feature, a failed
//! storage change is recovered from a checkpoint; without it, storage can remain
//! partially modified on failure.

use crate::{
    Addon, Component, Dependency, EnvVars, Runner, Slot, Umu, WineBridge, Wrappers, error::Result,
};

use super::{Inner, PrefixState};
use crate::{Job, Operation, PrefixError, Stage};
use std::sync::Arc;

/// Provides mutable access to a prefix's editable state.
///
/// Changes remain private until the edit operation completes. The
/// [`PrefixBackend`](crate::PrefixBackend) cannot be changed through this API.
///
/// # Examples
///
/// ```
/// use bottles_core::Edit;
///
/// fn configure(edit: &mut Edit<'_>) {
///     edit.env_vars().insert("WINEDEBUG".into(), "-all".into());
///     edit.wrappers().mangohud.enabled = true;
/// }
/// ```
pub struct Edit<'a> {
    pub(crate) draft: &'a mut PrefixState,
}

impl Edit<'_> {
    /// Selects the Wine or Proton runtime used on the next startup.
    ///
    /// Selecting the same identifier preserves the frozen record in the draft.
    pub fn set_runner(&mut self, runner: Addon<Runner>) {
        if self.draft.runner.id() != runner.id() {
            self.draft.runner = runner;
        }
    }

    /// Selects the WineBridge service used on the next startup.
    ///
    /// Selecting the same identifier preserves the frozen record in the draft.
    pub fn set_winebridge(&mut self, winebridge: Addon<WineBridge>) {
        if self.draft.winebridge.id() != winebridge.id() {
            self.draft.winebridge = winebridge;
        }
    }

    /// Selects or clears the optional UMU launcher.
    ///
    /// Selecting the same identifier preserves the frozen record in the draft.
    pub fn set_umu(&mut self, umu: Option<Addon<Umu>>) {
        if self.draft.umu.as_ref().map(Addon::id) != umu.as_ref().map(Addon::id) {
            self.draft.umu = umu;
        }
    }

    /// Selects `component` for the slot declared by that component.
    ///
    /// Other slots are unchanged. Selecting the same addon identifier preserves
    /// the frozen record already stored in the draft.
    pub fn set_component(&mut self, component: Addon<Component>) {
        if self
            .draft
            .component(component.slot())
            .is_some_and(|old| old.id() == component.id())
        {
            return;
        }
        self.draft.components.insert(component.slot(), component);
    }

    /// Removes the component selected in `slot`.
    ///
    /// Requirement validation is deferred until the edit callback returns, so a
    /// caller may remove and replace a required component in one edit.
    ///
    /// # Errors
    ///
    /// Returns [`PrefixError::ComponentNotInstalled`] if `slot` is empty.
    pub fn remove_component(&mut self, slot: Slot) -> Result<()> {
        self.draft
            .components
            .remove(&slot)
            .ok_or(PrefixError::ComponentNotInstalled(slot))?;
        Ok(())
    }

    /// Appends `dependency` in installation order if it is not already selected.
    ///
    /// Duplicate addon identifiers are ignored. Dependencies cannot be removed
    /// or reordered through the edit API.
    pub fn add_dependency(&mut self, dependency: Addon<Dependency>) {
        if self.draft.dependency(dependency.id()).is_none() {
            self.draft.dependencies.push(dependency);
        }
    }

    /// Returns user-set environment variables used on the next startup.
    ///
    /// These values override variables contributed by components and dependencies.
    pub fn env_vars(&mut self) -> &mut EnvVars {
        &mut self.draft.env_vars
    }

    /// Returns host command wrappers used on the next startup.
    pub fn wrappers(&mut self) -> &mut Wrappers {
        &mut self.draft.wrappers
    }
}

impl Inner {
    /// Runs a coordinated edit and publishes it only after application succeeds.
    ///
    /// Runtime and prefix changes require a stopped prefix. Runtime-only
    /// changes save directly for Standard prefixes. Virgo rebuilds the
    /// composition when the runner changes; UMU and WineBridge changes only save.
    /// Storage changes are [checkpointed](Self::checkpointed) together with the
    /// save that follows them.
    ///
    /// Cancellation is observed before a change starts and at backend-specific
    /// safe points. Once backend application succeeds, saving and publication
    /// finish without another cancellation check.
    ///
    /// # Errors
    ///
    /// Returns an error when the callback rejects the draft, validation fails,
    /// the prefix is running during a software change, cancellation is
    /// observed at a backend safe point, or backend application/persistence fails.
    pub(crate) fn edit<R: Send + 'static>(
        self: &Arc<Self>,
        callback: impl FnOnce(&mut Edit<'_>) -> Result<R> + Send + 'static,
    ) -> Operation<R> {
        let prefix = self.clone();
        Operation::new(move |job| async move {
            let _control = job.cancellable(prefix.control.lock()).await?;
            let previous = prefix.state()?;
            let mut draft = previous.as_ref().clone();
            let result = callback(&mut Edit { draft: &mut draft })?;
            draft.validate()?;
            if draft == *previous {
                return Ok(result);
            }
            let before = previous.as_ref();
            let after = &draft;
            let prefix_changed =
                before.components != after.components || before.dependencies != after.dependencies;
            let runner_changed = before.runner != after.runner;
            let umu_changed = before.umu != after.umu;
            let runtime_changed =
                runner_changed || umu_changed || before.winebridge != after.winebridge;
            if prefix_changed || runtime_changed {
                if prefix.bridge().await?.is_some() {
                    return Err(PrefixError::MustBeStopped.into());
                }
                job.transition(Stage::Stopping)?;
                prefix.stop_locked().await?;
            }
            // Once application succeeds, finish saving even if cancellation arrives.
            if prefix.storage.changes(before, after) {
                prefix
                    .checkpointed(&job, async {
                        prefix
                            .storage
                            .apply(before, after, &prefix.root, &job)
                            .await?;
                        prefix.save(&draft).await
                    })
                    .await?;
            } else {
                if runner_changed || umu_changed {
                    after.runner.load_runner(after.umu.as_ref()).await?;
                }
                prefix.save(&draft).await?;
            }
            prefix.publish(draft);
            Ok(result)
        })
    }

    /// Runs `change` on stopped storage, recovering the prefix if it fails.
    ///
    /// With the `fvs` feature, a checkpoint of the whole root is captured first
    /// and restored when `change` fails. Without it, a failed change is not
    /// rolled back and can leave storage inconsistent with the published state.
    ///
    /// # Errors
    ///
    /// Returns cancellation before the checkpoint, checkpoint failures, the
    /// error of `change` after a successful recovery, or
    /// [`PrefixError::Rollback`] if recovery also fails.
    async fn checkpointed(
        &self,
        job: &Job,
        change: impl Future<Output = Result<()>>,
    ) -> Result<()> {
        #[cfg(feature = "fvs")]
        {
            use super::history;

            job.transition(Stage::Checkpointing)?;
            let checkpoint = history::capture(
                &self.root,
                history::AUTO_CHECKPOINT_MESSAGE.into(),
                false,
                &self.context,
                job.progress(),
            )
            .await?;
            history::recover(
                change.await,
                &self.root,
                &checkpoint,
                &self.context,
                job.progress(),
            )
            .await
        }
        #[cfg(not(feature = "fvs"))]
        {
            let _ = job;
            change.await
        }
    }
}
