use tokio::sync::MutexGuard;
use uuid::Uuid;

use super::{
    bottle::{Bottle, BottleState, Program},
    error::BottleError,
};
use crate::{Context, error::Result, wrapper::Wrappers};

#[must_use = "edits do nothing unless committed"]
pub struct BottleEdit<'a> {
    state: MutexGuard<'a, BottleState>,
    cx: Context,
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

impl<'a> BottleEdit<'a> {
    pub(super) fn new(state: MutexGuard<'a, BottleState>, cx: Context) -> Self {
        Self {
            state,
            cx,
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

    pub async fn commit(self) -> Result<()> {
        let BottleEdit {
            mut state,
            cx,
            changes,
        } = self;
        Bottle::update(&mut state, &cx, async move |state| {
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
                        if program.name.trim().is_empty() || program.executable.trim().is_empty() {
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
            Ok(())
        })
        .await
    }
}
