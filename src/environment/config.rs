//! Persisted execution settings shared by all environment owners.

use super::EnvironmentError;
use crate::{
    Addon, AddonError, Component, Dependency, EnvVars, Requirement, Slot, Wrappers, error::Result,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use strum::IntoEnumIterator;
use uuid::Uuid;

/// Execution settings embedded in a bottle or standalone program's saved state.
/// Selections preserve runtime contributions independently of installation inputs.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct EnvironmentState {
    /// Component releases pinned to their occupied slots.
    pub components: HashMap<Slot, Addon<Component>>,
    /// Installed dependencies in installation order.
    pub dependencies: Vec<Addon<Dependency>>,
    #[serde(default, skip_serializing_if = "EnvVars::is_empty")]
    pub env_vars: EnvVars,
    #[serde(default)]
    pub wrappers: Wrappers,
}

impl EnvironmentState {
    /// Resolve the downloaded runtime releases for a new environment.
    pub(crate) fn new(runner: Uuid, addons: &crate::Addons) -> Result<Self> {
        let runner_component = addons
            .component(runner)
            .ok_or(crate::AddonError::NotFound(runner))?;
        if runner_component.slot() != Slot::Runner {
            return Err(EnvironmentError::InvalidComponentSlot {
                component: runner_component.id(),
                required: Slot::Runner,
            }
            .into());
        }
        let winebridge = addons.latest_component(Slot::WineBridge);
        let needs_umu = runner_component
            .requirements()
            .contains(&Requirement::Slot(Slot::Umu));
        let umu = needs_umu
            .then(|| addons.latest_component(Slot::Umu))
            .flatten();
        let mut missing = Vec::new();
        if winebridge.is_none() {
            missing.push(Requirement::Slot(Slot::WineBridge));
        }
        if needs_umu && umu.is_none() {
            missing.push(Requirement::Slot(Slot::Umu));
        }
        if !missing.is_empty() {
            return Err(EnvironmentError::RequiresAddon {
                required_by: None,
                requirements: missing,
            }
            .into());
        }
        let winebridge = winebridge.unwrap(); // Safe to unwrap since we just checked it above
        let mut components = HashMap::from([
            (Slot::WineBridge, Addon::from(winebridge.as_ref())),
            (Slot::Runner, Addon::from(runner_component.as_ref())),
        ]);
        if let Some(umu) = umu {
            components.insert(Slot::Umu, Addon::from(umu.as_ref()));
        }
        let config = EnvironmentState {
            components,
            dependencies: Vec::new(),
            env_vars: Default::default(),
            wrappers: Default::default(),
        };
        config.validate()?;
        Ok(config)
    }

    /// Select a downloaded component and pair a runner with UMU when required.
    pub(crate) fn set_component(&mut self, id: Uuid, addons: &crate::Addons) -> Result<()> {
        let component = addons
            .component(id)
            .ok_or(crate::AddonError::NotFound(id))?;
        if self
            .component(component.slot())
            .is_some_and(|old| old.id() == id)
        {
            return Ok(());
        }
        let needs_umu = component
            .requirements()
            .contains(&crate::Requirement::Slot(Slot::Umu));
        if needs_umu && self.umu().is_none() {
            let umu = addons.latest_component(Slot::Umu).ok_or_else(|| {
                crate::EnvironmentError::RequiresAddon {
                    required_by: Some(id),
                    requirements: vec![crate::Requirement::Slot(Slot::Umu)],
                }
            })?;
            self.components
                .insert(Slot::Umu, crate::Addon::from(umu.as_ref()));
        }
        self.components
            .insert(component.slot(), crate::Addon::from(component.as_ref()));
        if component.slot() == Slot::Runner && !needs_umu {
            self.components.remove(&Slot::Umu);
        }
        Ok(())
    }

    pub(crate) fn remove_component(&mut self, slot: Slot) -> Result<()> {
        self.components
            .remove(&slot)
            .ok_or(EnvironmentError::ComponentNotInstalled(slot))?;
        Ok(())
    }

    /// Select a downloaded dependency unless it is already selected.
    pub(crate) fn add_dependency(&mut self, id: Uuid, addons: &crate::Addons) -> Result<()> {
        if self.dependency(id).is_none() {
            let dependency = addons.dependency(id).ok_or(AddonError::NotFound(id))?;
            self.dependencies.push(Addon::from(dependency.as_ref()));
        }
        Ok(())
    }

    /// Validate edited selections before runtime or prefix work.
    pub(crate) fn validate_edit(&self, previous: &Self, addons: &crate::Addons) -> Result<()> {
        self.validate()?;
        for slot in Slot::iter() {
            let old = previous.component(slot);
            let new = self.component(slot);
            if old == new {
                continue;
            }
            if let Some(new) = new {
                let downloaded = addons
                    .component(new.id())
                    .ok_or(AddonError::NotFound(new.id()))?;
                if Addon::from(downloaded.as_ref()) != *new {
                    return Err(EnvironmentError::InvalidEdit(
                        "component selection must match its downloaded release",
                    )
                    .into());
                }
            }
        }
        for new in &self.dependencies {
            if self
                .dependencies
                .iter()
                .filter(|addon| addon.id() == new.id())
                .count()
                != 1
            {
                return Err(EnvironmentError::InvalidEdit(
                    "a dependency may only be selected once",
                )
                .into());
            }
            if previous.dependency(new.id()) == Some(new) {
                continue;
            }
            let downloaded = addons
                .dependency(new.id())
                .ok_or(AddonError::NotFound(new.id()))?;
            if Addon::from(downloaded.as_ref()) != *new {
                return Err(EnvironmentError::InvalidEdit(
                    "dependency selection must match its downloaded release",
                )
                .into());
            }
        }

        Ok(())
    }

    /// Prefix-contributing components in fixed slot order.
    pub(crate) fn ordered_components(&self) -> impl Iterator<Item = &Addon<Component>> {
        Slot::iter()
            .filter(|slot| !slot.is_runtime())
            .filter_map(|slot| self.component(slot))
    }

    /// Combines saved addon contributions in selection order, then applies bottle overrides.
    pub(crate) fn effective_env_vars(&self) -> EnvVars {
        let mut vars = EnvVars::default();
        for addon in self.ordered_components() {
            vars.extend(addon.env_vars().clone());
        }
        for addon in &self.dependencies {
            vars.extend(addon.env_vars().clone());
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
        for (name, value) in self.env_vars.iter() {
            if name.is_empty() || name.contains(['=', '\0']) {
                return Err(EnvironmentError::InvalidEnvironmentName(name.into()).into());
            }
            if value.contains('\0') {
                return Err(EnvironmentError::InvalidEnvironmentValue(name.into()).into());
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
