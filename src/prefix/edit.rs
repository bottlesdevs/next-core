//! Coordinated edits to a prefix configuration.
//!
//! [`Edit`] is exposed only inside a prefix's edit callback. Changes are made to
//! a private draft, validated as a whole, applied to storage, persisted, and
//! published only after the callback succeeds. With the `fvs` feature, a failed
//! storage change is recovered from a checkpoint; without it, storage can remain
//! partially modified on failure.

use crate::{
    Addon, Component, Dependency, EnvVars, InstalledAddon, Runner, SettingValue, Slot, Umu,
    WineBridge, Wrappers, error::Result,
};
use uuid::Uuid;

use super::{Prefix, PrefixState};
use crate::{Job, Operation, PrefixError, Stage};

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
    /// Another release keeps the runner's stored setting values.
    pub fn set_runner(&mut self, runner: Addon<Runner>) {
        if self.draft.runner.id() != runner.id() {
            self.draft.runner.addon = runner;
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
    /// the frozen record already stored in the draft. Replacing another release
    /// in the slot keeps its stored setting values.
    pub fn set_component(&mut self, component: Addon<Component>) {
        match self.draft.components.get_mut(&component.slot()) {
            Some(old) if old.id() == component.id() => {}
            Some(old) => old.addon = component,
            None => {
                self.draft
                    .components
                    .insert(component.slot(), InstalledAddon::new(component));
            }
        }
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
            self.draft
                .dependencies
                .push(InstalledAddon::new(dependency));
        }
    }

    /// Sets setting `key` of the selected runner, component, or dependency
    /// with release UUID `addon`.
    ///
    /// Setting the default removes the stored value. Settings apply on the
    /// next startup and may change while Wine is running.
    ///
    /// # Errors
    ///
    /// Returns [`PrefixError::AddonNotSelected`] if no runner, component, or
    /// dependency has UUID `addon`, [`PrefixError::UnknownSetting`] if the
    /// release does not declare `key`, or [`PrefixError::InvalidSettingValue`]
    /// if the setting does not accept `value`.
    pub fn set_setting(
        &mut self,
        addon: Uuid,
        key: &str,
        value: impl Into<SettingValue>,
    ) -> Result<()> {
        let value = value.into();
        let draft = &mut *self.draft;
        if draft.runner.id() == addon {
            return draft.runner.set(key, value);
        }
        if let Some(component) = draft.components.values_mut().find(|c| c.id() == addon) {
            return component.set(key, value);
        }
        if let Some(dependency) = draft.dependencies.iter_mut().find(|d| d.id() == addon) {
            return dependency.set(key, value);
        }
        Err(PrefixError::AddonNotSelected(addon).into())
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

impl Prefix {
    /// Creates an operation that edits a private draft of the latest state.
    ///
    /// The final draft is validated, applied, saved, and then published once.
    /// Software selection changes require a stopped prefix; startup
    /// settings may change while Wine is running. An unchanged draft is not saved.
    /// Any shutdown begun for the edit finishes before cancellation is returned.
    /// Once application succeeds, persistence and publication are not cancelled.
    /// With the `fvs` feature, a checkpoint is captured before storage changes,
    /// and a failure after it recovers the previous files before returning.
    /// Without it, failure or cancellation can leave files changed while the
    /// previous state remains published.
    ///
    /// # Errors
    ///
    /// Awaiting the operation returns errors from the callback, validation,
    /// cancellation, persistence, software application, or storage recovery.
    /// [`PrefixError::MustBeStopped`](crate::PrefixError::MustBeStopped)
    /// is returned when software changes are requested while Wine is running.
    ///
    /// # Examples
    ///
    /// ```
    /// # use bottles_core::Prefix;
    /// # fn prepare(prefix: &Prefix) {
    /// let quiet = prefix.edit(|edit| {
    ///     edit.env_vars().insert("WINEDEBUG".into(), "-all".into());
    ///     Ok(())
    /// });
    /// # let _ = quiet;
    /// # }
    /// ```
    #[tracing::instrument(skip_all, fields(prefix = %self.id()))]
    pub fn edit<R: Send + 'static>(
        &self,
        callback: impl FnOnce(&mut Edit<'_>) -> Result<R> + Send + 'static,
    ) -> Operation<R> {
        let prefix = self.clone();
        Operation::new(move |job| async move {
            let _control = job.cancellable(prefix.0.control.lock()).await?;
            let previous = prefix.state()?;
            let mut draft = previous.as_ref().clone();
            let result = callback(&mut Edit { draft: &mut draft })?;
            draft.validate()?;
            if draft == *previous {
                return Ok(result);
            }
            let before = previous.as_ref();
            let after = &draft;
            let storage_changed = prefix.0.storage.changes(before, after);
            let runner_changed = before.runner.addon != after.runner.addon;
            let umu_changed = before.umu != after.umu;
            let runtime_changed =
                runner_changed || umu_changed || before.winebridge != after.winebridge;
            if storage_changed || runtime_changed {
                if prefix.bridge().await?.is_some() {
                    return Err(PrefixError::MustBeStopped.into());
                }
                job.transition(Stage::Stopping)?;
                prefix.stop_locked().await?;
            }
            // Once application succeeds, finish saving even if cancellation arrives.
            if storage_changed {
                prefix
                    .checkpointed(&job, async {
                        prefix
                            .0
                            .storage
                            .apply(before, after, &prefix.0.root, &job)
                            .await?;
                        prefix.save(&draft).await
                    })
                    .await?;
            } else {
                if runner_changed || umu_changed {
                    after.runner.runner(after.umu.as_ref())?;
                }
                prefix.save(&draft).await?;
            }
            prefix.publish(draft);
            tracing::info!(storage_changed, runtime_changed, "prefix edited");
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
                &self.0.root,
                history::AUTO_CHECKPOINT_MESSAGE.into(),
                false,
                &self.0.fvs.client,
                job.progress(),
            )
            .await?;
            history::recover(
                change.await,
                &self.0.root,
                &checkpoint,
                &self.0.fvs.client,
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
