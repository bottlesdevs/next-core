//! Persisted environment snapshots and executable configuration.
//!
//! [`EnvironmentConfig`] freezes addon selections, ordering, environment
//! overrides, and command wrappers. [`State`] pairs that configuration with the
//! owner-specific data stored by a bottle or standalone program.

use super::EnvironmentError;
use crate::{Addon, Component, Dependency, EnvVars, Requirement, Slot, Wrappers, error::Result};
use next_config::Config;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use strum::IntoEnumIterator;
use uuid::Uuid;

/// Describes the software and launch behavior of one environment.
///
/// Component and dependency entries are frozen [`Addon`] records. They preserve
/// the selected recipe even if the shared catalog later changes.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct EnvironmentConfig {
    /// Component release selected for each occupied [`Slot`].
    pub components: HashMap<Slot, Addon<Component>>,
    /// Dependency releases in installation order.
    pub dependencies: Vec<Addon<Dependency>>,
    /// Owner-level environment variables applied after addon variables.
    #[serde(default, skip_serializing_if = "EnvVars::is_empty")]
    pub env_vars: EnvVars,
    /// Host command wrappers applied when `WineBridge` is started.
    #[serde(default)]
    pub wrappers: Wrappers,
}

impl EnvironmentConfig {
    /// Creates the initial configuration from its required runtime components.
    pub(crate) fn new(
        runner: Addon<Component>,
        winebridge: Addon<Component>,
        umu: Option<Addon<Component>>,
    ) -> Self {
        let mut components =
            HashMap::from([(Slot::WineBridge, winebridge), (Slot::Runner, runner)]);
        if let Some(umu) = umu {
            components.insert(Slot::Umu, umu);
        }
        Self {
            components,
            dependencies: Vec::new(),
            env_vars: Default::default(),
            wrappers: Default::default(),
        }
    }

    /// Replaces the component in its declared slot.
    ///
    /// Selecting the same addon identifier is a no-op, preserving the frozen
    /// record already stored in the environment.
    pub(crate) fn set_component(&mut self, component: Addon<Component>) {
        if self
            .component(component.slot())
            .is_some_and(|old| old.id() == component.id())
        {
            return;
        }
        self.components.insert(component.slot(), component);
    }

    /// Removes the component occupying `slot`.
    ///
    /// # Errors
    ///
    /// Returns [`EnvironmentError::ComponentNotInstalled`] when `slot` is empty.
    pub(crate) fn remove_component(&mut self, slot: Slot) -> Result<()> {
        self.components
            .remove(&slot)
            .ok_or(EnvironmentError::ComponentNotInstalled(slot))?;
        Ok(())
    }

    /// Appends a dependency unless the same addon identifier is already selected.
    pub(crate) fn add_dependency(&mut self, dependency: Addon<Dependency>) {
        if self.dependency(dependency.id()).is_none() {
            self.dependencies.push(dependency);
        }
    }

    /// Iterates prefix-contributing components in [`Slot`] declaration order.
    pub(crate) fn ordered_components(&self) -> impl Iterator<Item = &Addon<Component>> {
        Slot::iter()
            .filter(|slot| !slot.is_runtime())
            .filter_map(|slot| self.component(slot))
    }

    /// Resolves the environment variables used to start `WineBridge`.
    ///
    /// Component variables are applied in slot order, followed by dependencies
    /// in installation order and finally [`Self::env_vars`]. Later declarations
    /// override earlier values.
    pub(crate) fn effective_env_vars(&self) -> EnvVars {
        let mut vars = EnvVars::default();
        for addon in Slot::iter().filter_map(|slot| self.component(slot)) {
            addon.extend_env_vars(&mut vars);
        }
        for addon in &self.dependencies {
            addon.extend_env_vars(&mut vars);
        }
        vars.extend(self.env_vars.clone());
        vars
    }

    /// Returns the runner frozen into this configuration.
    ///
    /// Catalog refreshes do not replace this value.
    ///
    /// # Panics
    ///
    /// Panics if an unvalidated configuration does not contain [`Slot::Runner`].
    pub fn runner(&self) -> &Addon<Component> {
        self.component(Slot::Runner)
            .expect("persisted environment configuration is validated")
    }

    /// Returns the `WineBridge` release frozen into this configuration.
    ///
    /// # Panics
    ///
    /// Panics if an unvalidated configuration does not contain [`Slot::WineBridge`].
    pub fn winebridge(&self) -> &Addon<Component> {
        self.component(Slot::WineBridge)
            .expect("persisted environment configuration is validated")
    }

    /// Returns the selected UMU launcher, if one is configured.
    pub fn umu(&self) -> Option<&Addon<Component>> {
        self.component(Slot::Umu)
    }

    /// Returns the component occupying `slot`, if the slot is selected.
    pub fn component(&self, slot: Slot) -> Option<&Addon<Component>> {
        self.components.get(&slot)
    }

    /// Returns the dependency with the requested addon `id`, if selected.
    pub fn dependency(&self, id: Uuid) -> Option<&Addon<Dependency>> {
        self.dependencies
            .iter()
            .find(|dependency| dependency.id() == id)
    }

    fn contains_addon_matching(&self, requirement: &Requirement) -> bool {
        self.components
            .values()
            .any(|component| component.satisfies(requirement))
            || self
                .dependencies
                .iter()
                .any(|dependency| dependency.satisfies(requirement))
    }

    /// Verifies placement, uniqueness, required runtimes, and addon requirements.
    ///
    /// # Errors
    ///
    /// Returns [`EnvironmentError::InvalidEdit`] for duplicate dependencies,
    /// [`EnvironmentError::InvalidComponentSlot`] for a misplaced component, or
    /// [`EnvironmentError::RequiresAddon`] for any unsatisfied requirement.
    pub(crate) fn validate(&self) -> Result<()> {
        let mut dependencies = HashSet::new();
        for addon in &self.dependencies {
            if !dependencies.insert(addon.id()) {
                return Err(EnvironmentError::InvalidEdit(
                    "a dependency may only be selected once",
                )
                .into());
            }
        }
        for (slot, component) in &self.components {
            if component.slot() != *slot {
                return Err(EnvironmentError::InvalidComponentSlot {
                    component: component.id(),
                    required: *slot,
                }
                .into());
            }
        }

        let missing = [Slot::WineBridge, Slot::Runner]
            .into_iter()
            .filter(|slot| self.component(*slot).is_none())
            .map(Requirement::Slot)
            .collect::<Vec<_>>();
        if !missing.is_empty() {
            return Err(EnvironmentError::RequiresAddon {
                required_by: None,
                requirements: missing,
            }
            .into());
        }

        for (id, requirements) in self
            .components
            .values()
            .map(|addon| (addon.id(), addon.requirements()))
            .chain(
                self.dependencies
                    .iter()
                    .map(|addon| (addon.id(), addon.requirements())),
            )
        {
            let missing = requirements
                .iter()
                .filter(|requirement| !self.contains_addon_matching(requirement))
                .cloned()
                .collect::<Vec<_>>();
            if !missing.is_empty() {
                return Err(EnvironmentError::RequiresAddon {
                    required_by: Some(id),
                    requirements: missing,
                }
                .into());
            }
        }
        Ok(())
    }
}

/// Captures one immutable, published view of an environment.
///
/// `T` is owner-specific state such as bottle metadata or a standalone program
/// specification. Acquired snapshots remain readable after later edits or deletion.
#[derive(Clone, Debug, serde::Deserialize, PartialEq, serde::Serialize)]
pub struct State<T> {
    /// Stable identifier, which must match the environment directory name.
    pub(crate) id: Uuid,
    /// Frozen executable configuration for this publication.
    pub(crate) config: EnvironmentConfig,
    /// Bottle- or program-specific state published with the configuration.
    pub(crate) data: T,
}

impl<T> State<T> {
    /// Returns the stable environment identifier.
    pub fn id(&self) -> Uuid {
        self.id
    }

    /// Returns the executable configuration stored in this snapshot.
    pub fn config(&self) -> &EnvironmentConfig {
        &self.config
    }
}

impl<T: serde::Serialize + serde::de::DeserializeOwned + 'static> Config for State<T> {
    const VERSION: u32 = 1;
}
