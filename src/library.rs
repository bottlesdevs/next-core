//! Installed programs, caller-driven remote refresh, and local snapshot search.

#[cfg(feature = "fvs")]
use crate::{Program, ProgramManager};

use std::sync::Arc;

use bottles_plugin_host::{LoadedPlugin, PluginInterface, Plugins};
use futures_core::Stream;
#[cfg(feature = "fvs")]
use futures_util::stream;
use futures_util::{StreamExt, stream::FuturesUnordered};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{
    Bottle, BottleManager, Operation, ProfileError, Profiles, ProgramSpec, StorefrontAccount,
    bottle::error::BottleError,
    error::{Error, Result},
};

pub use bottles_plugin_host::OwnedGame;

/// Remote games or a source failure for one captured account link.
/// The caller owns this snapshot and decides when to replace it.
pub struct AccountLibrary {
    pub account: StorefrontAccount,
    pub games: Result<Vec<OwnedGame>>,
}

/// A live, non-persisted projection of bottle registrations and standalone programs.
#[derive(Clone)]
pub struct Library {
    profiles: Profiles,
    plugins: Arc<Plugins>,
    bottles: BottleManager,
    #[cfg(feature = "fvs")]
    programs: ProgramManager,
}

impl Library {
    pub(crate) fn new(
        profiles: Profiles,
        plugins: Arc<Plugins>,
        bottles: BottleManager,
        #[cfg(feature = "fvs")] programs: ProgramManager,
    ) -> Self {
        Self {
            profiles,
            plugins,
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

    /// Capture the profile's accounts on first poll and refresh library-capable sources.
    /// Source errors are returned with their accounts; account-only providers are omitted.
    /// Cancellation drains started work through credential persistence before returning.
    /// Dropping the operation abandons that work; use `cancel().await` to finish safely.
    pub fn refresh(&self, profile_id: Uuid) -> Operation<Vec<AccountLibrary>> {
        let library = self.clone();
        Operation::new(move |_, cancellation| async move {
            let snapshot = library.profiles.snapshot();
            let accounts = snapshot
                .profile(profile_id)
                .ok_or(ProfileError::NotFound(profile_id))?
                .accounts()
                .to_vec();
            let mut pending = accounts
                .into_iter()
                .map(|account| {
                    let library = &library;
                    let cancellation = &cancellation;
                    async move {
                        let games = match library
                            .refresh_account(profile_id, &account, cancellation)
                            .await
                        {
                            Ok(Some(games)) => Ok(games),
                            Ok(None) => return None,
                            Err(error) => Err(error),
                        };
                        Some(AccountLibrary { account, games })
                    }
                })
                .collect::<FuturesUnordered<_>>();
            let mut sources = Vec::new();
            while let Some(source) = pending.next().await {
                if let Some(source) = source {
                    sources.push(source);
                }
            }
            if cancellation.is_cancelled() {
                return Err(Error::Cancelled);
            }
            Ok(sources)
        })
    }

    async fn refresh_account(
        &self,
        profile_id: Uuid,
        account: &StorefrontAccount,
        cancellation: &CancellationToken,
    ) -> Result<Option<Vec<OwnedGame>>> {
        let provider = cancellation
            .run_until_cancelled(get_library(&self.plugins, &account.provider.id))
            .await
            .ok_or(Error::Cancelled)??;
        let Some(provider) = provider else {
            return Ok(None);
        };
        let plugin = &provider;
        let access = self
            .profiles
            .refresh_credential(
                profile_id,
                account.link_id,
                cancellation,
                |account_id, credential| async move {
                    bottles_plugin_host::storefront::authenticate(
                        plugin,
                        &account_id,
                        credential.as_deref(),
                    )
                    .await
                    .map_err(|message| {
                        ProfileError::Provider {
                            provider: account.provider.id.clone(),
                            message,
                        }
                        .into()
                    })
                },
            )
            .await?;
        // Retain the same revision across authentication, credential persistence, and enumeration.
        if cancellation.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let games = bottles_plugin_host::storefront::list_games(
            &provider,
            &account.identity.account_id,
            &access,
        )
        .await;
        if cancellation.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let games = games.map_err(|message| ProfileError::Provider {
            provider: account.provider.id.clone(),
            message,
        })?;
        Ok(Some(games))
    }

    /// Filter current installed programs and a caller-owned remote snapshot.
    /// This performs no authentication, network requests, or credential writes.
    /// Inspect each source's `games` result separately to display refresh failures.
    pub fn search(&self, query: &str, remote: &[AccountLibrary]) -> Vec<SearchEntry> {
        let query = query.trim().to_lowercase();
        let installed = self.list().into_iter().filter_map(|installed| {
            let (title, source_name) = installed.summary().ok()?;
            Some(SearchEntry {
                title,
                source_name,
                source: SearchSource::Installed(installed),
            })
        });
        let remote = remote.iter().flat_map(|source| {
            source.games.iter().flatten().map(|game| SearchEntry {
                title: game.title.clone(),
                source_name: source.account.provider.name.to_string(),
                source: SearchSource::Storefront {
                    link_id: source.account.link_id,
                    game_id: game.id.clone(),
                },
            })
        });
        installed
            .chain(remote)
            .filter(|entry| entry.matches(&query))
            .collect()
    }
}

/// Account-only providers have no library to refresh.
async fn get_library(plugins: &Plugins, id: &str) -> Result<Option<LoadedPlugin>> {
    if id == "native:steam" {
        return Ok(None);
    }
    let package_id = id
        .strip_prefix("plugin:")
        .ok_or_else(|| ProfileError::ProviderNotFound(id.into()))?;
    let plugin = plugins.load(package_id).await?;
    Ok(plugin
        .info
        .exports(PluginInterface::LibraryProvider)
        .then_some(plugin))
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
    Storefront { link_id: Uuid, game_id: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn storefront_search_matches_title_and_source_without_using_them_as_identity() {
        let link_id = Uuid::new_v4();
        let entry = SearchEntry {
            title: "Fortnite".into(),
            source_name: "Epic Games Store".into(),
            source: SearchSource::Storefront {
                link_id,
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
                link_id: actual_link,
                game_id,
            } if *actual_link == link_id
                && game_id == "fortnite"
        ));
    }
}
