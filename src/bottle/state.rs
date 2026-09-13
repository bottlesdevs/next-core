//! Persisted bottle state and the shared bottle handle.

use std::{collections::HashMap, sync::Arc};

use futures_core::Stream;
use next_config::Config;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{EnvironmentState, PrefixBackend, ProgramSpec, error::Result};

/// An immutable snapshot of a bottle's published configuration.
///
/// Snapshots are returned by [`Bottle::state`]  [`Bottle::watch`].
/// They remain valid after the bottle changes or is deleted;
/// their getters continue to return the values recorded when that particular
/// snapshot was published. Obtain another snapshot to observe later changes.
/// Component payload locations are derived from their UUIDs.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, Config)]
#[config(version = 1)]
pub struct BottleState {
    pub(crate) id: Uuid,
    pub(crate) name: String,
    pub(crate) backend: PrefixBackend,
    pub(crate) environment: EnvironmentState,
    #[serde(default)]
    pub(crate) programs: HashMap<Uuid, ProgramSpec>,
}

impl crate::environment::EnvironmentOwnerState for BottleState {
    const FILE_NAME: &'static str = "bottle.toml";
    fn id(&self) -> Uuid {
        self.id
    }
    fn backend(&self) -> PrefixBackend {
        self.backend
    }
    fn environment(&self) -> &EnvironmentState {
        &self.environment
    }
    fn environment_mut(&mut self) -> &mut EnvironmentState {
        &mut self.environment
    }
    fn validate(&self) -> Result<()> {
        BottleState::validate(self)
    }
}

impl BottleState {
    pub(crate) fn validate(&self) -> Result<()> {
        self.environment.validate()?;
        for launch in self.programs.values() {
            launch.validate()?;
        }
        Ok(())
    }

    /// Returns the bottle's stable identity.
    pub fn id(&self) -> Uuid {
        self.id
    }

    /// Returns the display name.
    ///
    /// Names are not identities and need not be unique.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the execution settings shared by every registration in this bottle.
    pub fn environment(&self) -> &EnvironmentState {
        &self.environment
    }

    /// Iterates over registered programs in unspecified order.
    pub fn programs(&self) -> impl Iterator<Item = (Uuid, &ProgramSpec)> {
        self.programs.iter().map(|(id, launch)| (*id, launch))
    }

    /// Returns the backend fixed when this bottle was created.
    pub fn backend(&self) -> PrefixBackend {
        self.backend
    }

    /// Returns the registered program with identity `id`.
    pub fn program(&self, id: Uuid) -> Option<&ProgramSpec> {
        self.programs.get(&id)
    }
}

/// A live bottle handle. Clones share state and coordination; dropping a handle
/// does not stop Wine. Operations fail after deletion, including identity access.
#[derive(Clone)]
pub struct Bottle(pub(crate) Arc<crate::environment::Environment<BottleState>>);

impl Bottle {
    pub fn id(&self) -> Result<Uuid> {
        Ok(self.state()?.id())
    }
    pub fn state(&self) -> Result<Arc<BottleState>> {
        self.0.state()
    }

    /// Observe current state and later publications. Deletion ends the stream.
    pub fn watch(&self) -> impl Stream<Item = Arc<BottleState>> + Send + 'static + use<> {
        self.0.watch()
    }
}
