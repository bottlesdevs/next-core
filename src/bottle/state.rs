//! Persisted bottle state and the shared bottle handle.

use std::{
    collections::HashMap,
    hash::{Hash, Hasher},
    sync::Arc,
};

use futures_core::Stream;
use next_config::Config;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::error::BottleError;
use crate::{EnvironmentState, PrefixBackend, error::Result};

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
        self.environment.validate_requirements()?;
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

impl Hash for Bottle {
    fn hash<H: Hasher>(&self, state: &mut H) {
        Arc::as_ptr(&self.0).hash(state);
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

/// A persisted, immutable Windows launch definition registered with a bottle.
#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProgramSpec {
    name: String,
    executable: String,
    /// Command-line fragments joined with spaces before launch.
    ///
    /// Callers must include any Windows quoting needed for spaces or special
    /// characters inside an argument. An empty entry adds only a separator and
    /// does not produce an empty Windows argument.
    #[serde(default)]
    args: Vec<String>,
    /// Windows working directory, or WineBridge's inherited directory when absent.
    #[serde(default)]
    working_directory: Option<String>,
    /// Passed to WineBridge's `CREATE_NEW_CONSOLE` launch option.
    #[serde(default)]
    new_console: bool,
}

impl ProgramSpec {
    pub(crate) fn validate(&self) -> Result<()> {
        if self.name.trim().is_empty() {
            return Err(BottleError::InvalidProgram("name must not be blank".into()).into());
        }
        if self.executable.trim().is_empty() {
            return Err(BottleError::InvalidProgram("executable must not be blank".into()).into());
        }
        if self
            .working_directory
            .as_ref()
            .is_some_and(|path| path.trim().is_empty())
        {
            return Err(
                BottleError::InvalidProgram("working directory must not be blank".into()).into(),
            );
        }
        Ok(())
    }

    /// Creates a launch definition with default launch options.
    pub fn new(name: impl Into<String>, executable: impl Into<String>) -> Result<Self> {
        let name = name.into();
        let executable = executable.into();
        let program = Self {
            name,
            executable,
            args: Vec::new(),
            working_directory: None,
            new_console: false,
        };
        program.validate()?;
        Ok(program)
    }

    /// Replaces the Windows command-line fragments passed at launch.
    pub fn with_args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.args = args.into_iter().map(Into::into).collect::<Vec<_>>();
        self
    }

    /// Sets the Windows working directory used at launch.
    pub fn with_working_directory(mut self, working_directory: impl Into<String>) -> Result<Self> {
        let working_directory = working_directory.into();
        if working_directory.trim().is_empty() {
            return Err(
                BottleError::InvalidProgram("working directory must not be blank".into()).into(),
            );
        }
        self.working_directory = Some(working_directory);
        Ok(self)
    }

    /// Controls WineBridge's `CREATE_NEW_CONSOLE` launch option.
    pub fn with_new_console(mut self, new_console: bool) -> Self {
        self.new_console = new_console;
        self
    }

    /// Changes the display name; the owner validates it when saving an edit.
    pub fn rename(&mut self, name: impl Into<String>) {
        self.name = name.into();
    }

    /// Returns the display name preserved at registration.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the Windows executable path passed to WineBridge.
    pub fn executable(&self) -> &str {
        &self.executable
    }

    /// Returns the command-line fragments passed at launch.
    pub fn args(&self) -> &[String] {
        &self.args
    }

    /// Returns the configured Windows working directory, when present.
    pub fn working_directory(&self) -> Option<&str> {
        self.working_directory.as_deref()
    }

    /// Reports whether launch requests create a new console.
    pub fn new_console(&self) -> bool {
        self.new_console
    }
}
