//! The persisted record of a prefix's contents and startup settings.
//!
//! [`PrefixState`] records the prefix backend, the frozen addon selections
//! applied to the prefix, and the settings used to start its runtime.

use super::{PrefixBackend, PrefixError};
use crate::{
    Addon, Component, Dependency, EnvVars, InstalledAddon, Requirement, Runner, Slot, Umu,
    WineBridge, Wrappers, addons::InstallStep, error::Result,
};
use next_config::Config;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

/// Captures one immutable, published view of a prefix.
///
/// Addon selections are frozen records. They preserve their metadata and any
/// installation recipe even if the shared catalog later changes. The runner,
/// components, and dependencies also hold the setting values chosen for this
/// prefix. Acquired snapshots remain readable after later edits or deletion.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct PrefixState {
    /// Storage strategy chosen when the prefix was created.
    pub backend: PrefixBackend,
    /// Wine or Proton runtime selected for this prefix.
    pub runner: InstalledAddon<Runner>,
    /// WineBridge service selected for this prefix.
    pub winebridge: Addon<WineBridge>,
    /// Optional UMU launcher used by Proton runtimes.
    pub umu: Option<Addon<Umu>>,
    /// Component release selected for each occupied [`Slot`].
    pub components: HashMap<Slot, InstalledAddon<Component>>,
    /// Dependency releases in installation order.
    pub dependencies: Vec<InstalledAddon<Dependency>>,
    /// User-set environment variables applied after addon variables.
    #[serde(default, skip_serializing_if = "EnvVars::is_empty")]
    pub env_vars: EnvVars,
    /// Host command wrappers applied when `WineBridge` is started.
    #[serde(default)]
    pub wrappers: Wrappers,
}

impl PrefixState {
    /// Creates the initial state from its backend and selected runtime tools.
    pub(crate) fn new(
        backend: PrefixBackend,
        runner: Addon<Runner>,
        winebridge: Addon<WineBridge>,
        umu: Option<Addon<Umu>>,
    ) -> Self {
        Self {
            backend,
            runner: InstalledAddon::new(runner),
            winebridge,
            umu,
            components: HashMap::new(),
            dependencies: Vec::new(),
            env_vars: Default::default(),
            wrappers: Default::default(),
        }
    }

    /// Resolves the environment variables used to start `WineBridge`.
    ///
    /// Recipe declarations from components are applied first, followed by
    /// dependencies in installation order and finally [`Self::env_vars`]. Later
    /// declarations override earlier values; command-local variables are excluded.
    pub(crate) fn effective_env_vars(&self) -> EnvVars {
        let mut vars = EnvVars::default();
        let steps = self
            .components
            .values()
            .flat_map(|component| component.recipe())
            .chain(
                self.dependencies
                    .iter()
                    .flat_map(|dependency| dependency.recipe()),
            );
        for step in steps {
            if let InstallStep::SetEnvironment { name, value } = step {
                vars.insert(name.clone(), value.clone());
            }
        }
        vars.extend(self.env_vars.clone());
        vars
    }

    /// Returns the component occupying `slot`, if the slot is selected.
    pub fn component(&self, slot: Slot) -> Option<&InstalledAddon<Component>> {
        self.components.get(&slot)
    }

    /// Returns the dependency with the requested addon `id`, if selected.
    pub fn dependency(&self, id: Uuid) -> Option<&InstalledAddon<Dependency>> {
        self.dependencies
            .iter()
            .find(|dependency| dependency.id() == id)
    }

    fn contains_addon_matching(&self, requirement: &Requirement) -> bool {
        match requirement {
            Requirement::Slot(slot) => self.component(*slot).is_some(),
            Requirement::Name(name) => self
                .addon_metadata()
                .any(|(_, selected, _)| selected == name),
            Requirement::Id(id) => self
                .addon_metadata()
                .any(|(selected, _, _)| selected == *id),
        }
    }

    fn addon_metadata(&self) -> impl Iterator<Item = (Uuid, &str, &[Requirement])> {
        [self.runner.metadata(), self.winebridge.metadata()]
            .into_iter()
            .chain(self.umu.iter().map(Addon::metadata))
            .chain(
                self.components
                    .values()
                    .map(|component| component.metadata()),
            )
            .chain(
                self.dependencies
                    .iter()
                    .map(|dependency| dependency.metadata()),
            )
    }

    /// Returns whether `other` selects different component or dependency
    /// releases. Setting values are not compared.
    pub(crate) fn software_differs(&self, other: &Self) -> bool {
        fn releases(
            state: &PrefixState,
        ) -> (HashMap<Slot, &Addon<Component>>, Vec<&Addon<Dependency>>) {
            (
                state
                    .components
                    .iter()
                    .map(|(slot, component)| (*slot, &component.addon))
                    .collect(),
                state
                    .dependencies
                    .iter()
                    .map(|dependency| &dependency.addon)
                    .collect(),
            )
        }
        releases(self) != releases(other)
    }

    /// Verifies that every selected addon's requirements are satisfied.
    ///
    /// # Errors
    ///
    /// Returns [`PrefixError::RequiresAddon`] for any unsatisfied requirement.
    pub(crate) fn validate(&self) -> Result<()> {
        for (id, _, requirements) in self.addon_metadata() {
            let missing = requirements
                .iter()
                .filter(|requirement| !self.contains_addon_matching(requirement))
                .cloned()
                .collect::<Vec<_>>();
            if !missing.is_empty() {
                return Err(PrefixError::RequiresAddon {
                    required_by: id,
                    requirements: missing,
                }
                .into());
            }
        }
        Ok(())
    }
}

impl Config for PrefixState {
    const VERSION: u32 = 1;
}
