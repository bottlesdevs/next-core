//! Persisted execution settings shared by all environment owners.

use super::EnvironmentError;
use crate::{Addon, Component, Dependency, EnvVars, Requirement, Slot, Wrappers, error::Result};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use strum::IntoEnumIterator;
use uuid::Uuid;

/// Execution settings embedded in a bottle or standalone program's saved state.
/// Selections preserve complete frozen recipes independently of shared payloads.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct EnvironmentConfig {
    /// Component releases pinned to their occupied slots.
    pub components: HashMap<Slot, Addon<Component>>,
    /// Installed dependencies in installation order.
    pub dependencies: Vec<Addon<Dependency>>,
    #[serde(default, skip_serializing_if = "EnvVars::is_empty")]
    pub env_vars: EnvVars,
    #[serde(default)]
    pub wrappers: Wrappers,
}

impl EnvironmentConfig {
    /// Construct a selection from caller-supplied runtime records.
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

    /// Change only the supplied component's slot, preserving an already-selected identity.
    pub(crate) fn set_component(&mut self, component: Addon<Component>) {
        if self
            .component(component.slot())
            .is_some_and(|old| old.id() == component.id())
        {
            return;
        }
        self.components.insert(component.slot(), component);
    }

    pub(crate) fn remove_component(&mut self, slot: Slot) -> Result<()> {
        self.components
            .remove(&slot)
            .ok_or(EnvironmentError::ComponentNotInstalled(slot))?;
        Ok(())
    }

    /// Append the supplied dependency unless its identity is already selected.
    pub(crate) fn add_dependency(&mut self, dependency: Addon<Dependency>) {
        if self.dependency(dependency.id()).is_none() {
            self.dependencies.push(dependency);
        }
    }

    /// Prefix-contributing components in fixed slot order.
    pub(crate) fn ordered_components(&self) -> impl Iterator<Item = &Addon<Component>> {
        Slot::iter()
            .filter(|slot| !slot.is_runtime())
            .filter_map(|slot| self.component(slot))
    }

    /// Apply all components in slot order, then dependencies in installation order,
    /// then owner overrides. Later declarations win.
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

    /// Returns the runner recorded when this snapshot was published.
    ///
    /// Catalog refreshes do not replace this value.
    pub fn runner(&self) -> &Addon<Component> {
        self.component(Slot::Runner)
            .expect("persisted environment configuration is validated")
    }

    /// Returns the exact WineBridge release selected for this environment.
    pub fn winebridge(&self) -> &Addon<Component> {
        self.component(Slot::WineBridge)
            .expect("persisted environment configuration is validated")
    }

    /// Returns the selected UMU release, if this runtime uses one.
    pub fn umu(&self) -> Option<&Addon<Component>> {
        self.component(Slot::Umu)
    }

    /// Returns the component occupying `slot`, if any.
    pub fn component(&self, slot: Slot) -> Option<&Addon<Component>> {
        self.components.get(&slot)
    }

    /// Returns the installed dependency with this release identifier.
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
