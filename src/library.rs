//! Aggregate installed programs and one-shot library search.

use std::sync::Arc;

use futures_core::Stream;
use futures_util::{
    StreamExt,
    stream::{self, FuturesUnordered},
};
use uuid::Uuid;

use crate::{
    Bottle, BottleManager, PluginId, PluginKind, Plugins, Profiles, Program,
    bottle::error::BottleError, credentials, error::Result,
};

/// A live, non-persisted projection of programs registered in managed bottles.
#[derive(Clone)]
pub struct Library {
    bottles: BottleManager,
    profiles: Arc<Profiles>,
    plugins: Arc<Plugins>,
}

impl Library {
    pub(crate) fn new(
        bottles: BottleManager,
        profiles: Arc<Profiles>,
        plugins: Arc<Plugins>,
    ) -> Self {
        Self {
            bottles,
            profiles,
            plugins,
        }
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
            .filter_map(|account| {
                let provider_id = account.provider.id.clone();
                let plugin = self
                    .plugins
                    .contribution(&provider_id, PluginKind::StorefrontLibraryProvider)?;
                let account_id = account.identity.account_id.clone();
                let source_name = plugin.manifest.name.clone();
                let query = query.clone();

                Some(async move {
                    let credential = match credentials::load(&provider_id, profile_id).await {
                        Ok(Some(credential)) => credential,
                        Ok(None) => {
                            tracing::warn!(
                                provider = %provider_id,
                                profile = %profile_id,
                                "storefront credential is missing"
                            );
                            return Vec::new();
                        }
                        Err(error) => {
                            tracing::warn!(
                                provider = %provider_id,
                                profile = %profile_id,
                                "failed to load storefront credential: {error}"
                            );
                            return Vec::new();
                        }
                    };
                    let listed = match plugin
                        .runtime
                        .list_games(&account_id, Some(&credential))
                        .await
                    {
                        Ok(listed) => listed,
                        Err(error) => {
                            tracing::warn!(
                                provider = %provider_id,
                                profile = %profile_id,
                                "failed to list storefront games: {error}"
                            );
                            return Vec::new();
                        }
                    };

                    if let Some(updated) = listed.updated_credential.as_deref()
                        && let Err(error) =
                            credentials::save(&provider_id, profile_id, updated).await
                    {
                        tracing::warn!(
                            provider = %provider_id,
                            profile = %profile_id,
                            "failed to save refreshed storefront credential: {error}"
                        );
                    }

                    listed
                        .games
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
                })
            })
            .collect::<FuturesUnordered<_>>();

        let installed = self
            .list()
            .into_iter()
            .filter_map(|installed| {
                let state = installed.bottle.state().ok()?;
                let program = state.program(installed.program_id)?;
                Some(SearchEntry {
                    title: program.name().to_owned(),
                    source_name: state.name().to_owned(),
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
    /// A launchable program currently registered in a bottle.
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
