use std::sync::Arc;
use uuid::Uuid;

use super::{
    bottle::{Bottle, BottleInner, Program},
    error::BottleError,
};
use crate::{
    error::Result,
    wrapper::{gamescope::GamescopeConfig, mangohud::MangoHudConfig},
};

#[must_use = "edits do nothing unless committed"]
pub struct BottleEdit {
    inner: Arc<BottleInner>,
    changes: Vec<Change>,
}

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
    pub(super) fn new(inner: Arc<BottleInner>) -> Self {
        Self {
            inner,
            changes: Vec::new(),
        }
    }

    pub fn rename(&mut self, name: impl Into<String>) -> &mut Self {
        self.changes.push(Change::Rename(name.into()));
        self
    }

    pub fn set_env(&mut self, key: &str, value: &str) -> &mut Self {
        self.changes
            .push(Change::SetEnv(key.to_owned(), value.to_owned()));
        self
    }

    pub fn unset_env(&mut self, key: &str) -> &mut Self {
        self.changes.push(Change::UnsetEnv(key.to_owned()));
        self
    }

    pub fn add_program(&mut self, program: Program) -> &mut Self {
        self.changes.push(Change::AddProgram(program));
        self
    }

    pub fn remove_program(&mut self, id: Uuid) -> &mut Self {
        self.changes.push(Change::RemoveProgram(id));
        self
    }

    pub fn set_gamescope(&mut self, config: GamescopeConfig) -> &mut Self {
        self.changes.push(Change::SetGamescope(config));
        self
    }

    pub fn set_mangohud(&mut self, config: MangoHudConfig) -> &mut Self {
        self.changes.push(Change::SetMangoHud(config));
        self
    }

    pub async fn commit(self) -> Result<()> {
        let BottleEdit { inner, changes } = self;
        let bottle = Bottle::from_inner(inner);
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
                            if program.name.trim().is_empty()
                                || program.executable.trim().is_empty()
                            {
                                return Err(BottleError::InvalidProgram.into());
                            }
                            state.programs.push(program);
                        }
                        Change::RemoveProgram(id) => {
                            let index = state
                                .programs
                                .iter()
                                .position(|program| program.id == id)
                                .ok_or(BottleError::ProgramNotFound(id))?;
                            state.programs.remove(index);
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
