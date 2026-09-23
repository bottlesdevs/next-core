//! Persisted bottle state and the shared bottle handle.

use std::{collections::HashMap, sync::Arc};

use futures_core::Stream;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{PrefixBackend, ProgramSpec, State, error::Result};

/// Bottle-specific configuration. Execution settings live in [`State`].
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct BottleData {
    pub(crate) name: String,
    pub(crate) backend: PrefixBackend,
    #[serde(default)]
    pub(crate) programs: HashMap<Uuid, ProgramSpec>,
}

/// An immutable snapshot of a bottle's complete published configuration.
pub type BottleState = State<BottleData>;

impl crate::environment::BackendSource for BottleData {
    fn backend(&self) -> PrefixBackend {
        self.backend
    }
}

impl State<BottleData> {
    /// Returns the display name.
    ///
    /// Names are not identities and need not be unique.
    pub fn name(&self) -> &str {
        &self.data.name
    }

    /// Iterates over registered programs in unspecified order.
    pub fn programs(&self) -> impl Iterator<Item = (Uuid, &ProgramSpec)> {
        self.data.programs.iter().map(|(id, launch)| (*id, launch))
    }

    /// Returns the backend fixed when this bottle was created.
    pub fn backend(&self) -> PrefixBackend {
        self.data.backend
    }

    /// Returns the registered program with identity `id`.
    pub fn program(&self, id: Uuid) -> Option<&ProgramSpec> {
        self.data.programs.get(&id)
    }
}

/// A live bottle handle. Clones share state and coordination; dropping a handle
/// does not stop Wine. Operations fail after deletion, including identity access.
#[derive(Clone)]
pub struct Bottle(pub(crate) Arc<crate::environment::Environment<BottleData>>);

impl crate::environment::Managed for Bottle {
    type Data = BottleData;

    fn from_environment(environment: Arc<crate::environment::Environment<BottleData>>) -> Self {
        Self(environment)
    }

    fn environment(&self) -> &crate::environment::Environment<BottleData> {
        &self.0
    }
}

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
