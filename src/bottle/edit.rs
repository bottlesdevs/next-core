//! Batched edits to persisted bottle configuration.

use uuid::Uuid;

use super::{
    error::BottleError,
    state::{Bottle, Program},
};
use crate::{
    error::Result,
    wrapper::{gamescope::GamescopeConfig, mangohud::MangoHudConfig},
};

#[must_use = "edits do nothing unless committed"]
/// A pending batch of configuration changes for a [`Bottle`].
///
/// Builder methods only queue changes. [`commit`](Self::commit) applies them in
/// order to a draft of the latest state available when the commit acquires
/// exclusive access; it does not capture the state that existed when
/// [`Bottle::edit`] was called. The draft is published only after it has been
/// persisted. Dropping an edit without committing it has no effect.
pub struct BottleEdit {
    bottle: Bottle,
    changes: Vec<Change>,
}

/// One mutation queued by [`BottleEdit`]; vector order is commit order.
enum Change {
    Rename(String),
    SetEnv(String, String),
    UnsetEnv(String),
    AddProgram(Program),
    RemoveProgram(Uuid),
    SetGamescope(GamescopeConfig),
    SetMangoHud(MangoHudConfig),
}

impl BottleEdit {
    pub(super) fn new(bottle: Bottle) -> Self {
        Self {
            bottle,
            changes: Vec::new(),
        }
    }

    /// Changes the bottle's display name.
    ///
    /// Names are stored verbatim, may be empty, and need not be unique.
    pub fn rename(&mut self, name: impl Into<String>) -> &mut Self {
        self.changes.push(Change::Rename(name.into()));
        self
    }

    /// Sets an environment variable for future WineBridge starts.
    ///
    /// This does not change an already-running WineBridge. Call [`Bottle::stop`]
    /// before the next bridge-backed operation to apply it immediately.
    /// Values stored here are applied after runner-provided variables, so they
    /// can override values such as `WINEPREFIX`, `WINEARCH`, and `PROTONPATH`.
    ///
    /// At commit time, names must be nonempty and contain neither `=` nor NUL;
    /// values must not contain NUL. Case and whitespace are preserved, and
    /// lookup is case-sensitive.
    pub fn set_env(&mut self, key: &str, value: &str) -> &mut Self {
        self.changes
            .push(Change::SetEnv(key.to_owned(), value.to_owned()));
        self
    }

    /// Removes an environment variable for future WineBridge starts.
    ///
    /// Removing a missing variable succeeds. Names have the same validation
    /// and case-sensitive matching rules as [`set_env`](Self::set_env).
    pub fn unset_env(&mut self, key: &str) -> &mut Self {
        self.changes.push(Change::UnsetEnv(key.to_owned()));
        self
    }

    /// Registers a program.
    pub fn add_program(&mut self, program: Program) -> &mut Self {
        self.changes.push(Change::AddProgram(program));
        self
    }

    /// Removes the program identified by `id`.
    ///
    /// The edit fails to commit if the program is not registered.
    pub fn remove_program(&mut self, id: Uuid) -> &mut Self {
        self.changes.push(Change::RemoveProgram(id));
        self
    }

    /// Replaces the Gamescope configuration used for future WineBridge starts.
    ///
    /// If WineBridge is already running, stop the bottle after committing so
    /// that the next bridge-backed operation starts it with the new wrapper.
    pub fn set_gamescope(&mut self, config: GamescopeConfig) -> &mut Self {
        self.changes.push(Change::SetGamescope(config));
        self
    }

    /// Replaces the MangoHud configuration used for future WineBridge starts.
    ///
    /// If WineBridge is already running, stop the bottle after committing so
    /// that the next bridge-backed operation starts it with the new wrapper.
    pub fn set_mangohud(&mut self, config: MangoHudConfig) -> &mut Self {
        self.changes.push(Change::SetMangoHud(config));
        self
    }

    /// Validates, persists, and publishes all queued changes.
    ///
    /// Changes are applied in call order, so a later change may supersede an
    /// earlier one. Concurrent commits serialize and each starts from the
    /// latest persisted state. If validation or persistence fails, no new
    /// state snapshot is published. An empty edit is still persisted, but an
    /// unchanged state does not notify [`Bottle::watch`].
    ///
    /// # Errors
    ///
    /// Returns an error for a deleted bottle, a missing program removal, an
    /// invalid environment variable, or a persistence failure.
    pub async fn commit(self) -> Result<()> {
        let BottleEdit { bottle, changes } = self;
        bottle
            .update(async move |state, _| {
                for change in changes {
                    match change {
                        Change::Rename(name) => state.name = name,
                        Change::SetEnv(key, value) => {
                            if key.is_empty() || key.contains('=') || key.contains('\0') {
                                return Err(BottleError::InvalidEnvironmentName(key).into());
                            }
                            if value.contains('\0') {
                                return Err(BottleError::InvalidEnvironmentValue(key).into());
                            }
                            state.environment.insert(key, value);
                        }
                        Change::UnsetEnv(key) => {
                            if key.is_empty() || key.contains('=') || key.contains('\0') {
                                return Err(BottleError::InvalidEnvironmentName(key).into());
                            }
                            state.environment.remove(&key);
                        }
                        Change::AddProgram(program) => {
                            state.programs.insert(program.id(), program);
                        }
                        Change::RemoveProgram(id) => {
                            state
                                .programs
                                .remove(&id)
                                .ok_or(BottleError::ProgramNotFound(id))?;
                        }
                        Change::SetGamescope(config) => state.wrappers.gamescope = config,
                        Change::SetMangoHud(config) => state.wrappers.mangohud = config,
                    }
                }
                Ok(())
            })
            .await
    }
}
