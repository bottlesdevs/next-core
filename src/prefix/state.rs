//! The persisted record of a prefix's contents and startup settings.
//!
//! [`PrefixState`] records the prefix backend, the frozen addon selections
//! applied to the prefix, and the settings used to start its runtime.

use super::{ArchLimit, PrefixBackend, PrefixError};
use crate::{
    Addon, Component, Dependency, EnvVars, InstalledAddon, Requirement, Runner, RunnerFlavor, Slot,
    Umu, WineBridge, Wrappers, error::Result, runner::Runner as RunnerBackend,
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
    /// Windows architecture chosen when the prefix was created.
    pub arch: WineArch,
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
        arch: WineArch,
        runner: Addon<Runner>,
        winebridge: Addon<WineBridge>,
        umu: Option<Addon<Umu>>,
    ) -> Self {
        Self {
            backend,
            arch,
            runner: InstalledAddon::new(runner),
            winebridge,
            umu,
            components: HashMap::new(),
            dependencies: Vec::new(),
            env_vars: Default::default(),
            wrappers: Default::default(),
        }
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

    /// Returns the runner this prefix runs, with its UMU.
    ///
    /// # Errors
    ///
    /// Returns an error if the runner is Proton and no UMU is selected.
    pub(crate) fn runner(&self) -> Result<Box<dyn RunnerBackend>> {
        self.runner.runner(self.umu.as_ref())
    }

    /// Verifies that the runner and backend support the architecture and that
    /// every selected addon's requirements are satisfied.
    ///
    /// # Errors
    ///
    /// Returns [`PrefixError::UnsupportedArch`] as [`WineArch::check`] does, or
    /// [`PrefixError::RequiresAddon`] for any unsatisfied requirement.
    pub(crate) fn validate(&self) -> Result<()> {
        self.arch.check(self.backend, self.runner.flavor())?;
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

/// Selects the Windows architecture of a Wine prefix.
///
/// Wine fixes the architecture when it first initializes a prefix, so it is
/// chosen at creation and cannot be changed afterwards.
#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    serde::Deserialize,
    Eq,
    PartialEq,
    serde::Serialize,
    strum::Display,
    strum::IntoStaticStr,
)]
#[serde(rename_all = "lowercase")]
#[strum(serialize_all = "lowercase")]
pub enum WineArch {
    /// A 64-bit prefix, which also runs 32-bit programs.
    #[default]
    Win64,
    /// A 32-bit prefix. Proton runners and Virgo storage do not support it;
    /// see [`check`](Self::check).
    Win32,
}

impl WineArch {
    /// Checks that a prefix of `backend` running a `flavor` runner can use this
    /// architecture.
    ///
    /// # Errors
    ///
    /// Returns [`PrefixError::UnsupportedArch`] for a 32-bit prefix with a
    /// Proton runner or Virgo storage.
    pub fn check(
        self,
        backend: PrefixBackend,
        flavor: RunnerFlavor,
    ) -> std::result::Result<(), PrefixError> {
        let by = match (self, backend, flavor) {
            (Self::Win64, _, _) => return Ok(()),
            (Self::Win32, _, RunnerFlavor::Proton) => ArchLimit::Proton,
            (Self::Win32, PrefixBackend::Virgo, _) => ArchLimit::Virgo,
            (Self::Win32, PrefixBackend::Standard, RunnerFlavor::Wine) => return Ok(()),
        };
        Err(PrefixError::UnsupportedArch { arch: self, by })
    }
}
