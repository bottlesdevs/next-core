//! Bottle collection lifecycle backed by the shared environment registry.
use super::{Bottle, BottleData};
use crate::{
    Addon, Component, LibraryEntry, LibraryProvider, Manager, Operation, PrefixBackend,
    error::{Error, Result},
};
use std::collections::HashMap;
use uuid::Uuid;

#[async_trait::async_trait]
impl LibraryProvider for Manager<Bottle> {
    fn id(&self) -> &str {
        "bottles"
    }

    async fn list_entries(&self) -> Result<Vec<LibraryEntry>> {
        let mut entries = Vec::new();
        for state in self
            .list()
            .into_iter()
            .filter_map(|bottle| bottle.state().ok())
        {
            entries.extend(state.programs().map(|(id, program)| LibraryEntry {
                id: format!("{}/{id}", state.id()),
                title: program.name().to_owned(),
            }));
        }
        Ok(entries)
    }

    fn launch(&self, entry_id: &str) -> Result<Operation<()>> {
        let (bottle_id, program_id) =
            entry_id
                .split_once('/')
                .ok_or_else(|| Error::LibraryProvider {
                    provider: self.id().to_owned(),
                    message: "expected bottle UUID/program UUID".into(),
                })?;
        let parse_id = |id| {
            Uuid::parse_str(id).map_err(|error| Error::LibraryProvider {
                provider: self.id().to_owned(),
                message: error.to_string(),
            })
        };
        Ok(self
            .open(parse_id(bottle_id)?)?
            .launch_program(parse_id(program_id)?)
            .map(|_| ()))
    }
}

impl Manager<Bottle> {
    /// Creates a bottle using `runner` and the selected storage strategy.
    ///
    /// A new UUID is assigned when the operation starts;
    /// display names are stored verbatim, may be empty, and need not be unique.
    /// Callers supply the runner, WineBridge, and optional UMU records. Their slots
    /// and coexistence requirements are checked before creating files; missing
    /// requirements fail without selecting or downloading other owner components.
    /// Standard creation initializes Wine without FVS. Virgo
    /// creation builds missing shared layers before creating private storage and
    /// composing its registry and saving selections. Startup mounts prepared storage.
    /// Failures and cancellation observed while the operation remains polled remove the
    /// partially-created bottle directory on a best-effort basis. Dropping a
    /// started operation or a cleanup failure can leave a directory that a
    /// later library startup discovers.
    ///
    /// # Errors
    ///
    /// Returns [`crate::EnvironmentError::RequiresAddon`] for missing coexistence
    /// requirements before creating any files. Other service, I/O, and prefix
    /// creation failures are returned directly.
    pub fn create(
        &self,
        name: impl Into<String>,
        backend: PrefixBackend,
        runner: Addon<Component>,
        winebridge: Addon<Component>,
        umu: Option<Addon<Component>>,
    ) -> Operation<Bottle> {
        self.create_environment(
            BottleData {
                name: name.into(),
                backend,
                programs: HashMap::new(),
            },
            runner,
            winebridge,
            umu,
        )
    }
}
