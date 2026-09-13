//! Bottle registrations and forwarding to shared environment operations.
use super::{Bottle, error::BottleError};
use crate::{
    Operation, ProgramSpec, Slot,
    error::Result,
    proto::{DllOverride, DllOverrideMode, Process},
};
use uuid::Uuid;

impl Bottle {
    pub fn dll_overrides(&self) -> Operation<Vec<DllOverride>> {
        self.0.dll_overrides()
    }
    pub fn set_dll_override(&self, dll: impl Into<String>, mode: DllOverrideMode) -> Operation<()> {
        self.0.set_dll_override(dll.into(), mode)
    }
    pub fn unset_dll_override(&self, dll: impl Into<String>) -> Operation<()> {
        self.0.unset_dll_override(dll.into())
    }

    /// Resolve the registration under coordination before preparing or starting Wine.
    pub fn launch_program(&self, id: Uuid) -> Operation<u32> {
        self.0.launch(move |state| {
            let launch = state
                .program(id)
                .cloned()
                .ok_or(BottleError::ProgramNotFound(id))?;
            Ok((id, launch))
        })
    }
    /// Launch an unregistered definition with a caller-selected process-group UUID.
    pub fn launch(&self, id: Uuid, launch: ProgramSpec) -> Operation<u32> {
        self.0.launch(move |_| Ok((id, launch)))
    }
    pub async fn processes(&self) -> Result<Vec<Process>> {
        self.0.processes().await
    }
    pub async fn kill_program(&self, id: Uuid) -> Result<()> {
        self.0
            .kill(move |state| {
                state.program(id).ok_or(BottleError::ProgramNotFound(id))?;
                Ok(id)
            })
            .await
    }
    pub async fn stop(&self) -> Result<()> {
        self.0.stop().await
    }
    pub fn set_component(&self, id: Uuid) -> Operation<()> {
        self.0.set_component(id)
    }
    pub fn remove_component(&self, slot: Slot) -> Operation<()> {
        self.0.remove_component(slot)
    }
    pub fn install(&self, id: Uuid) -> Operation<()> {
        self.0.install(id)
    }
}
