use uuid::Uuid;

use super::{
    bottle::{Bottle, BottleState, Program},
    error::BottleError,
};
use crate::{error::Result, wrapper::Wrappers};

#[must_use = "edits do nothing unless committed"]
pub struct BottleEdit {
    bottle: Bottle,
    changes: Vec<Change>,
}

enum Change {
    Rename(String),
    SetEnv(String, String),
    UnsetEnv(String),
    AddProgram(Program),
    RemoveProgram(Uuid),
    SetWrappers(Wrappers),
}

impl BottleEdit {
    pub(super) fn new(bottle: Bottle) -> Self {
        Self {
            bottle,
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

    pub fn set_wrappers(&mut self, wrappers: Wrappers) -> &mut Self {
        self.changes.push(Change::SetWrappers(wrappers));
        self
    }

    pub async fn commit(self) -> Result<Bottle> {
        let BottleEdit { bottle, changes } = self;
        let id = bottle.id();
        let cx = bottle.cx;
        let path = cx.directories().bottle(id).join("bottle.toml");
        let bottles_path = cx.directories().bottles();
        let state = cx
            .spawn_blocking(move || {
                let mut state: BottleState = next_config::load(&path)?;
                if state.id != id {
                    return Err(BottleError::IdMismatch {
                        expected: id,
                        actual: state.id,
                    }
                    .into());
                }

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
                        Change::SetWrappers(wrappers) => state.wrappers = wrappers,
                    }
                }

                for entry in std::fs::read_dir(bottles_path)? {
                    let candidate = entry?.path().join("bottle.toml");
                    if candidate != path && candidate.is_file() {
                        let other: BottleState = next_config::load(candidate)?;
                        if other.name == state.name {
                            return Err(BottleError::DuplicateName(state.name).into());
                        }
                    }
                }
                next_config::save(path, &state)?;
                Ok(state)
            })
            .await?;
        Ok(Bottle::from_state(state, cx))
    }
}
