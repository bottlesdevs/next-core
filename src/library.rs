//! Aggregate installed programs and one-shot library search.

use futures_core::Stream;
use futures_lite::{StreamExt, stream};
use uuid::Uuid;

use crate::{Bottle, BottleManager, Program, bottle::error::BottleError, error::Result};

/// A live, non-persisted projection of programs registered in managed bottles.
#[derive(Clone)]
pub struct Library {
    bottles: BottleManager,
}

impl Library {
    pub(crate) fn new(bottles: BottleManager) -> Self {
        Self { bottles }
    }

    /// Returns immutable handles for every currently registered program.
    ///
    /// This reads only current in-memory bottle snapshots. Ordering is
    /// unspecified, and bottles deleted during the snapshot are omitted.
    pub fn list(&self) -> Vec<LibraryItem> {
        self.bottles
            .list()
            .into_iter()
            .filter_map(|bottle| bottle.state().ok().map(|state| (bottle, state)))
            .flat_map(|(bottle, state)| {
                state
                    .programs()
                    .map(move |program| LibraryItem {
                        bottle: bottle.clone(),
                        program_id: program.id(),
                    })
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    /// Watches the bottle registry and yields the current [`list`](Self::list).
    ///
    /// The stream yields the current snapshot first. Slow consumers may miss
    /// intermediate generations and receive only the latest aggregate state.
    pub fn watch(&self) -> impl Stream<Item = Vec<LibraryItem>> + Send + 'static {
        let library = self.clone();
        self.bottles.watch().map(move |_| library.list())
    }

    /// Searches one snapshot of locally installed programs.
    ///
    /// An empty or whitespace-only query matches every program. Other queries
    /// use case-insensitive substring matching against program and bottle names.
    /// Each matching entry is emitted once, then the stream ends.
    pub fn search(
        &self,
        query: impl Into<String>,
    ) -> impl Stream<Item = SearchEntry> + Send + 'static {
        let query = query.into().trim().to_lowercase();
        stream::iter(self.list()).filter_map(move |installed| {
            let state = installed.bottle.state().ok()?;
            let program = state.program(installed.program_id)?;
            let matches = query.is_empty()
                || program.name().to_lowercase().contains(&query)
                || state.name().to_lowercase().contains(&query);
            matches.then(|| SearchEntry {
                title: program.name().to_owned(),
                subtitle: Some(state.name().to_owned()),
                actions: vec![SearchAction::Launch(installed)],
            })
        })
    }
}

/// A live reference to a registered program with actions bound to its bottle.
#[derive(Clone)]
pub struct LibraryItem {
    bottle: Bottle,
    program_id: Uuid,
}

impl LibraryItem {
    /// Returns the current launch definition.
    pub fn program(&self) -> Result<Program> {
        self.bottle
            .state()?
            .program(self.program_id)
            .cloned()
            .ok_or_else(|| BottleError::ProgramNotFound(self.program_id).into())
    }

    /// Launches the current registration.
    pub async fn launch(&self) -> Result<u32> {
        self.bottle.launch_program(self.program_id).await
    }

    /// Kills the current registration's process group.
    pub async fn kill(&self) -> Result<()> {
        self.bottle.kill_program(self.program_id).await
    }
}

/// One immutable library-search result.
#[derive(Clone)]
pub struct SearchEntry {
    title: String,
    subtitle: Option<String>,
    actions: Vec<SearchAction>,
}

impl SearchEntry {
    /// Returns the primary display title.
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Returns optional supporting text.
    pub fn subtitle(&self) -> Option<&str> {
        self.subtitle.as_deref()
    }

    /// Returns the typed actions currently available for this result.
    pub fn actions(&self) -> &[SearchAction] {
        &self.actions
    }
}

/// A typed action advertised by a [`SearchEntry`].
#[derive(Clone)]
#[non_exhaustive]
pub enum SearchAction {
    /// Launches one program currently registered in a bottle.
    Launch(LibraryItem),
}
