//! Installed, launchable programs.

#[cfg(feature = "fvs")]
use crate::{Program, ProgramManager};

use futures_core::Stream;
use futures_util::StreamExt;
#[cfg(feature = "fvs")]
use futures_util::stream;
use uuid::Uuid;

use crate::{
    Bottle, BottleManager, Operation, ProgramSpec, bottle::error::BottleError, error::Result,
};

/// A live, non-persisted projection of bottle registrations and standalone programs.
#[derive(Clone)]
pub struct Library {
    bottles: BottleManager,
    #[cfg(feature = "fvs")]
    programs: ProgramManager,
}

impl Library {
    pub(crate) fn new(
        bottles: BottleManager,
        #[cfg(feature = "fvs")] programs: ProgramManager,
    ) -> Self {
        Self {
            bottles,
            #[cfg(feature = "fvs")]
            programs,
        }
    }

    /// Returns immutable handles for every currently registered program.
    ///
    /// This reads only current in-memory owner states. Ordering is
    /// unspecified, and bottles deleted during the snapshot are omitted.
    pub fn list(&self) -> Vec<LibraryItem> {
        let items = self
            .bottles
            .list()
            .into_iter()
            .filter_map(|bottle| bottle.state().ok().map(|state| (bottle, state)))
            .flat_map(|(bottle, state)| {
                state
                    .programs()
                    .map(move |(id, _)| LibraryItem::Bottle {
                        bottle: bottle.clone(),
                        program_id: id,
                    })
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        #[cfg(feature = "fvs")]
        let items = {
            let mut items = items;
            items.extend(
                self.programs
                    .list()
                    .into_iter()
                    .filter(|program| program.state().is_ok())
                    .map(LibraryItem::Standalone),
            );
            items
        };
        items
    }

    /// Watches both installed registries and yields the current [`list`](Self::list).
    ///
    /// The stream yields the current snapshot first. Slow consumers may miss
    /// intermediate generations and receive only the latest aggregate state.
    /// Either registry may publish the same initial snapshot; neither initial
    /// event is skipped because it may include changes since the other was polled.
    pub fn watch(&self) -> impl Stream<Item = Vec<LibraryItem>> + Send + 'static + use<> {
        let library = self.clone();
        let changes = self.bottles.watch().map(|_| ());
        #[cfg(feature = "fvs")]
        let changes = stream::select(changes, self.programs.watch().map(|_| ()));
        changes.map(move |_| library.list())
    }
}

/// A live installed item with actions bound to its owning environment.
#[derive(Clone)]
pub enum LibraryItem {
    Bottle {
        bottle: Bottle,
        program_id: Uuid,
    },
    #[cfg(feature = "fvs")]
    Standalone(Program),
}

impl LibraryItem {
    /// Returns the latest launch definition, failing if the owner or registration is gone.
    pub fn program(&self) -> Result<ProgramSpec> {
        match self {
            Self::Bottle { bottle, program_id } => bottle
                .state()?
                .program(*program_id)
                .cloned()
                .ok_or_else(|| BottleError::ProgramNotFound(*program_id).into()),
            #[cfg(feature = "fvs")]
            Self::Standalone(program) => Ok(program.state()?.launch().clone()),
        }
    }

    /// Launch using the current definition and owning environment.
    pub fn launch(&self) -> Operation<u32> {
        match self {
            Self::Bottle { bottle, program_id } => bottle.launch_program(*program_id),
            #[cfg(feature = "fvs")]
            Self::Standalone(program) => program.launch(),
        }
    }

    /// Kill the installed item's process group without starting a stopped runtime.
    pub async fn kill(&self) -> Result<()> {
        match self {
            Self::Bottle { bottle, program_id } => bottle.kill_program(*program_id).await,
            #[cfg(feature = "fvs")]
            Self::Standalone(program) => program.kill().await,
        }
    }
}
