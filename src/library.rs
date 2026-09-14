//! Aggregate installed programs and one-shot library search.

#[cfg(feature = "fvs")]
use crate::{Program, ProgramManager};

use futures_core::Stream;
use futures_util::{
    StreamExt,
    stream::{self, FuturesUnordered},
};
use uuid::Uuid;

use crate::{
    Bottle, BottleManager, Operation, PluginId, Profiles, ProgramSpec, bottle::error::BottleError,
    error::Result,
};

/// A live, non-persisted projection of bottle registrations and standalone programs.
#[derive(Clone)]
pub struct Library {
    bottles: BottleManager,
    #[cfg(feature = "fvs")]
    programs: ProgramManager,
    profiles: Profiles,
}

impl Library {
    pub(crate) fn new(
        bottles: BottleManager,
        #[cfg(feature = "fvs")] programs: ProgramManager,
        profiles: Profiles,
    ) -> Self {
        Self {
            bottles,
            #[cfg(feature = "fvs")]
            programs,
            profiles,
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

    /// Searches installed programs and games owned by the selected profile.
    ///
    /// The selected profile and installed programs are snapshotted when this
    /// method is called. Storefront searches start when the returned stream is
    /// first polled, and results are emitted as their sources become ready.
    /// Storefront failures are logged and omitted so local and other storefront
    /// results remain available. An empty or whitespace-only query matches every
    /// entry, and result ordering is unspecified.
    pub fn search(
        &self,
        query: impl Into<String>,
    ) -> impl Stream<Item = SearchEntry> + Send + 'static {
        let query = query.into().trim().to_lowercase();
        let profile = self.profiles.selected();
        let profile_id = profile.id();
        let storefronts = profile
            .accounts()
            .iter()
            .cloned()
            .map(|account| {
                let profiles = self.profiles.clone();
                let query = query.clone();
                async move {
                    let provider_id = account.provider.id.clone();
                    let (source_name, games) =
                        match profiles.owned_games(profile_id, &account).await {
                            Ok(listed) => listed,
                            Err(error) => {
                                tracing::warn!(provider = %provider_id, profile = %profile_id,
                                "failed to list storefront games: {error}");
                                return Vec::new();
                            }
                        };
                    games
                        .into_iter()
                        .filter_map(|game| {
                            let entry = SearchEntry {
                                title: game.title,
                                source_name: source_name.clone(),
                                source: SearchSource::Storefront {
                                    profile_id,
                                    provider_id: provider_id.clone(),
                                    game_id: game.id,
                                },
                            };
                            entry.matches(&query).then_some(entry)
                        })
                        .collect()
                }
            })
            .collect::<FuturesUnordered<_>>();

        let installed = self
            .list()
            .into_iter()
            .filter_map(|installed| {
                let (title, source_name) = installed.summary().ok()?;
                Some(SearchEntry {
                    title,
                    source_name,
                    source: SearchSource::Installed(installed),
                })
            })
            .filter(|entry| entry.matches(&query))
            .collect::<Vec<_>>();

        stream::select(
            storefronts.flat_map_unordered(None, stream::iter),
            stream::iter(installed),
        )
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

    fn summary(&self) -> Result<(String, String)> {
        match self {
            Self::Bottle { bottle, program_id } => {
                let state = bottle.state()?;
                let launch = state
                    .program(*program_id)
                    .ok_or(BottleError::ProgramNotFound(*program_id))?;
                Ok((launch.name().into(), state.name().into()))
            }
            #[cfg(feature = "fvs")]
            Self::Standalone(program) => Ok((program.state()?.name().into(), "Standalone".into())),
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

/// One immutable library-search result.
#[derive(Clone)]
pub struct SearchEntry {
    title: String,
    source_name: String,
    source: SearchSource,
}

impl SearchEntry {
    /// Returns the primary display title.
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Returns the bottle or storefront display name.
    pub fn source_name(&self) -> &str {
        &self.source_name
    }

    /// Returns the result's actionable source and stable identity.
    pub fn source(&self) -> &SearchSource {
        &self.source
    }

    fn matches(&self, query: &str) -> bool {
        query.is_empty()
            || self.title.to_lowercase().contains(query)
            || self.source_name.to_lowercase().contains(query)
    }
}

/// The source and stable identity of a [`SearchEntry`].
#[derive(Clone)]
#[non_exhaustive]
pub enum SearchSource {
    /// A bottle registration or standalone program currently installed.
    Installed(LibraryItem),
    /// A game owned through one linked storefront account.
    Storefront {
        profile_id: Uuid,
        provider_id: PluginId,
        game_id: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn storefront_search_matches_title_and_source_without_using_them_as_identity() {
        let profile_id = Uuid::new_v4();
        let provider_id = PluginId::new("epic-games-store");
        let entry = SearchEntry {
            title: "Fortnite".into(),
            source_name: "Epic Games Store".into(),
            source: SearchSource::Storefront {
                profile_id,
                provider_id: provider_id.clone(),
                game_id: "fortnite".into(),
            },
        };

        assert!(entry.matches(""));
        assert!(entry.matches(&"FORT".to_lowercase()));
        assert!(entry.matches("epic"));
        assert!(!entry.matches("gog"));
        assert!(matches!(
            entry.source(),
            SearchSource::Storefront {
                profile_id: actual_profile,
                provider_id: actual_provider,
                game_id,
            } if *actual_profile == profile_id
                && actual_provider == &provider_id
                && game_id == "fortnite"
        ));
    }
}
