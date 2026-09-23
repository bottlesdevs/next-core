//! Draft edits applied together by the owner's coordinated operation.

use crate::{Addon, Component, Dependency, EnvVars, Slot, Wrappers, error::Result};

#[cfg(feature = "fvs")]
use super::history;
use super::{BackendSource, Environment, State, backend::standard};
use crate::{
    EnvironmentError, Operation, PrefixBackend, Progress, Stage, error::Error,
    winebridge::WineBridgeClient,
};
use std::sync::Arc;

/// A draft available only during an owner's edit callback.
/// Requirements are validated after the callback; identity and backend stay fixed.
pub struct Edit<'a, T> {
    pub(crate) draft: &'a mut State<T>,
}

impl<T> Edit<'_, T> {
    /// Select a frozen component without changing other slots. The same UUID is a no-op.
    pub fn set_component(&mut self, component: Addon<Component>) {
        self.draft.config.set_component(component);
    }

    /// Remove a selected component. An absent slot returns an error; requirements
    /// may be temporarily unsatisfied until the callback finishes.
    pub fn remove_component(&mut self, slot: Slot) -> Result<()> {
        self.draft.config.remove_component(slot)
    }

    /// Append a frozen dependency unless its UUID is already selected.
    /// Dependencies cannot be removed or reordered.
    pub fn add_dependency(&mut self, dependency: Addon<Dependency>) {
        self.draft.config.add_dependency(dependency);
    }

    /// Environment overrides used on the next startup.
    pub fn env_vars(&mut self) -> &mut EnvVars {
        &mut self.draft.config.env_vars
    }

    /// Command wrappers used on the next startup.
    pub fn wrappers(&mut self) -> &mut Wrappers {
        &mut self.draft.config.wrappers
    }
}

impl<T: BackendSource> Environment<T>
where
    State<T>: next_config::Config + Clone + PartialEq + Send + Sync,
{
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
