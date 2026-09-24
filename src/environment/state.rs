//! Persisted environment snapshots and executable configuration.
//!
//! [`EnvironmentConfig`] freezes addon selections, ordering, environment
//! overrides, and command wrappers. [`State`] pairs that configuration with the
//! owner-specific data stored by a bottle or standalone program.

use super::EnvironmentError;
use crate::{
    Addon, Component, Dependency, EnvVars, Requirement, Runner, Slot, Umu, WineBridge, Wrappers,
    addons::InstallStep, error::Result,
};
use next_config::Config;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use strum::IntoEnumIterator;
use uuid::Uuid;

/// Describes the software and launch behavior of one environment.
///
/// Addon selections are frozen records. They preserve their metadata and any
/// installation recipe even if the shared catalog later changes.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct EnvironmentConfig {
    /// Wine or Proton runtime selected for this environment.
    pub runner: Addon<Runner>,
    /// WineBridge service selected for this environment.
    pub winebridge: Addon<WineBridge>,
    /// Optional UMU launcher used by Proton runtimes.
    pub umu: Option<Addon<Umu>>,
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
    /// Creates the initial configuration from its selected runtime tools.
    pub(crate) fn new(
        runner: Addon<Runner>,
        winebridge: Addon<WineBridge>,
        umu: Option<Addon<Umu>>,
    ) -> Self {
        Self {
            runner,
            winebridge,
            umu,
            components: HashMap::new(),
            dependencies: Vec::new(),
            env_vars: Default::default(),
            wrappers: Default::default(),
        }
    }

    /// Iterates prefix-contributing components in [`Slot`] declaration order.
    pub(crate) fn ordered_components(&self) -> impl Iterator<Item = &Addon<Component>> {
        Slot::iter().filter_map(|slot| self.component(slot))
    }

    /// Resolves the environment variables used to start `WineBridge`.
    ///
    /// Recipe declarations are applied in component slot order, followed by
    /// dependencies in installation order and finally [`Self::env_vars`]. Later
    /// declarations override earlier values; command-local variables are excluded.
    pub(crate) fn effective_env_vars(&self) -> EnvVars {
        let mut vars = EnvVars::default();
        let steps = self
            .ordered_components()
            .flat_map(Addon::recipe)
            .chain(self.dependencies.iter().flat_map(Addon::recipe));
        for step in steps {
            if let InstallStep::SetEnvironment { name, value } = step {
                vars.insert(name.clone(), value.clone());
            }
        }
        vars.extend(self.env_vars.clone());
        vars
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
        match requirement {
            Requirement::Slot(slot) => self.components.contains_key(slot),
            Requirement::Name(name) => self
                .addon_metadata()
                .any(|(_, selected, _)| selected == name),
            Requirement::Id(id) => self
                .addon_metadata()
                .any(|(selected, _, _)| selected == *id),
        }
    }

    fn addon_metadata(&self) -> impl Iterator<Item = (Uuid, &str, &[Requirement])> {
        [
            (
                self.runner.id(),
                self.runner.name(),
                self.runner.requirements(),
            ),
            (
                self.winebridge.id(),
                self.winebridge.name(),
                self.winebridge.requirements(),
            ),
        ]
        .into_iter()
        .chain(
            self.umu
                .iter()
                .map(|addon| (addon.id(), addon.name(), addon.requirements())),
        )
        .chain(
            self.components
                .values()
                .map(|addon| (addon.id(), addon.name(), addon.requirements())),
        )
        .chain(
            self.dependencies
                .iter()
                .map(|addon| (addon.id(), addon.name(), addon.requirements())),
        )
    }

    /// Verifies placement, uniqueness, and addon requirements.
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

        for (id, _, requirements) in self.addon_metadata() {
            let missing = requirements
                .iter()
                .filter(|requirement| !self.contains_addon_matching(requirement))
                .cloned()
                .collect::<Vec<_>>();
            if !missing.is_empty() {
                return Err(EnvironmentError::RequiresAddon {
                    required_by: id,
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
