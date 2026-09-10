//! Persisted execution settings shared by all environment owners.

use super::{EnvironmentError, Storage};
use crate::{Addon, Component, Dependency, EnvVars, Requirement, Slot, Wrappers, error::Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

/// Execution settings embedded in a bottle or standalone program's saved state.
/// Storage retains resolved Virgo layers; live runtime resources are never persisted.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct EnvironmentConfig {
    pub storage: Storage,
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

    pub(crate) fn validate_requirements(&self) -> Result<()> {
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
