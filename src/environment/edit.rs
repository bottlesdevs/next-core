//! Coordinated edits to an environment configuration.
//!
//! An owner exposes [`Edit`] only inside its edit callback. Changes are made to
//! a private draft, validated as a whole, applied to storage, persisted, and
//! published only after the callback succeeds. Virgo edits recover from a failed
//! storage change; conventional prefixes can remain partially modified on failure.

use crate::{
    Addon, Component, Dependency, EnvVars, Runner, Slot, Umu, WineBridge, Wrappers, error::Result,
};

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
///
/// # Examples
///
/// ```
/// use bottles_core::Edit;
///
/// fn configure<T>(edit: &mut Edit<'_, T>) {
///     edit.env_vars().insert("WINEDEBUG".into(), "-all".into());
///     edit.wrappers().mangohud.enabled = true;
/// }
/// ```
pub struct Edit<'a, T> {
    pub(crate) draft: &'a mut State<T>,
}

impl<T> Edit<'_, T> {
    /// Selects the Wine or Proton runtime used on the next startup.
    ///
    /// Selecting the same identifier preserves the frozen record in the draft.
    pub fn set_runner(&mut self, runner: Addon<Runner>) {
        if self.draft.config.runner.id() != runner.id() {
            self.draft.config.runner = runner;
        }
    }

    /// Selects the WineBridge service used on the next startup.
    ///
    /// Selecting the same identifier preserves the frozen record in the draft.
    pub fn set_winebridge(&mut self, winebridge: Addon<WineBridge>) {
        if self.draft.config.winebridge.id() != winebridge.id() {
            self.draft.config.winebridge = winebridge;
        }
    }

    /// Selects or clears the optional UMU launcher.
    ///
    /// Selecting the same identifier preserves the frozen record in the draft.
    pub fn set_umu(&mut self, umu: Option<Addon<Umu>>) {
        if self.draft.config.umu.as_ref().map(Addon::id) != umu.as_ref().map(Addon::id) {
            self.draft.config.umu = umu;
        }
    }

    /// Selects `component` for the slot declared by that component.
    ///
    /// Other slots are unchanged. Selecting the same addon identifier preserves
    /// the frozen record already stored in the draft.
    pub fn set_component(&mut self, component: Addon<Component>) {
        let config = &mut self.draft.config;
        if config
            .component(component.slot())
            .is_some_and(|old| old.id() == component.id())
        {
            return;
        }
        config.components.insert(component.slot(), component);
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
        self.draft
            .config
            .components
            .remove(&slot)
            .ok_or(EnvironmentError::ComponentNotInstalled(slot))?;
        Ok(())
    }

    /// Appends `dependency` in installation order if it is not already selected.
    ///
    /// Duplicate addon identifiers are ignored. Dependencies cannot be removed
    /// or reordered through the edit API.
    pub fn add_dependency(&mut self, dependency: Addon<Dependency>) {
        if self.draft.config.dependency(dependency.id()).is_none() {
            self.draft.config.dependencies.push(dependency);
        }
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
    /// Runtime and prefix changes require a stopped environment. Runtime-only
    /// changes save directly for Standard prefixes. Virgo rebuilds the
    /// composition when the runner or UMU changes; WineBridge changes only save.
    /// Standard prefix changes apply in place without rollback; Virgo environments
    /// checkpoint and recover the owner if workspace preparation or persistence fails. A
    /// standard-prefix application or subsequent save failure can therefore leave
    /// prefix contents inconsistent with the last published configuration.
    ///
    /// Cancellation is observed before a change starts and at backend-specific
    /// safe points. Once backend application succeeds, saving and publication
    /// finish without another cancellation check.
    ///
    /// # Errors
    ///
    /// Returns an error when the callback rejects the draft, validation fails,
    /// the environment is running during a software change, cancellation is
    /// observed at a backend safe point, or backend application/persistence fails.
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
            let prefix_changed =
                before.components != after.components || before.dependencies != after.dependencies;
            let runner_changed = before.runner != after.runner;
            let umu_changed = before.umu != after.umu;
            let runtime_changed =
                runner_changed || umu_changed || before.winebridge != after.winebridge;
            if prefix_changed || runtime_changed {
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
            match previous.data.backend() {
                PrefixBackend::Standard if prefix_changed => {
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
                PrefixBackend::Standard if runner_changed || umu_changed => {
                    after
                        .runner
                        .load_runner(environment.context.directories(), after.umu.as_ref())
                        .await?;
                    environment.save(&draft).await?;
                }
                #[cfg(feature = "fvs")]
                PrefixBackend::Virgo if prefix_changed || runner_changed || umu_changed => {
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
                _ => environment.save(&draft).await?,
            }
            environment.publish(draft);
            Ok(result)
        })
    }
}
