//! Discovery and identity tracking for installed and hand-placed components.
//!
//! Hand-placed components use this layout beneath the application's
//! platform-local data directory:
//!
//! ```text
//! components/
//! ├── index.toml
//! ├── runners/
//! │   └── <version>/
//! ├── winebridge/
//! │   └── <version>/
//! ├── umu/
//! │   └── <version>/
//! ├── dxvk/
//! │   └── <version>/
//! ├── vkd3d/
//! │   └── <version>/
//! ├── nvapi/
//! │   └── <version>/
//! └── latency-flex/
//!     └── <version>/
//! ```
//!
//! Each category may contain multiple version directories. Unknown categories
//! and non-directory entries are ignored. An invalid component in a recognized
//! category aborts the scan.
//!
//! `components/index.toml` is library-managed state, not a user-authored
//! manifest. It preserves a hand-placed component's generated ID and version
//! only while its canonical path remains indexed. Moving it or losing the
//! index association gives it a new ID on the next scan. Catalog metadata takes
//! precedence when a catalog entry resolves to the same path. Otherwise the
//! version directory name becomes both the public name and version. Unlike
//! malformed cached catalogs, index load and parse errors abort addon discovery.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use futures_lite::StreamExt;
use next_config::Config;
use serde::{Deserialize, Serialize};
use uuid::{NonNilUuid, Uuid};

use super::{
    AddonError, Slot,
    catalog::{InternalRole, ItemKind},
};
use crate::{Directories, error::Result, runner::detect_runner_kind};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct IndexedComponent {
    id: NonNilUuid,
    version: String,
    path: PathBuf,
    #[serde(flatten)]
    kind: ItemKind,
}

impl IndexedComponent {
    pub(crate) fn id(&self) -> Uuid {
        self.id.get()
    }

    pub(crate) fn version(&self) -> &str {
        &self.version
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn kind(&self) -> ItemKind {
        self.kind
    }
}

/// Versioned contents of `components/index.toml`.
#[derive(Debug, Default, Deserialize, Eq, PartialEq, Serialize, Config)]
#[config(version = 1)]
struct ComponentIndex {
    #[serde(default, rename = "component")]
    components: Vec<IndexedComponent>,
}

impl ComponentIndex {
    /// Loads the index when its path can be confirmed as a regular file.
    ///
    /// Missing paths, non-files, and metadata lookup failures return `None`.
    /// Read, parse, and unsupported-version failures from a confirmed file are
    /// returned.
    async fn load(directories: &Directories) -> Result<Option<Self>> {
        let path = directories.component_index();
        if async_fs::metadata(&path)
            .await
            .is_ok_and(|entry| entry.is_file())
        {
            Ok(Some(next_config::load(path).await?))
        } else {
            Ok(None)
        }
    }

    async fn save(&self, directories: &Directories) -> Result<()> {
        next_config::save(directories.component_index(), self).await?;
        Ok(())
    }

    /// Records `component`, replacing entries with the same ID or path.
    pub(crate) async fn record(
        directories: &Directories,
        component: IndexedComponent,
    ) -> Result<()> {
        let mut index = Self::load(directories).await?.unwrap_or_default();
        index
            .components
            .retain(|entry| entry.id() != component.id() && entry.path() != component.path());
        index.components.push(component);
        index.save(directories).await
    }

    /// Removes `id` from the index and persists the resulting snapshot.
    ///
    /// Removing an unknown ID is successful but still writes the index.
    pub(crate) async fn remove(directories: &Directories, id: Uuid) -> Result<()> {
        let mut index = Self::load(directories).await?.unwrap_or_default();
        index.components.retain(|entry| entry.id() != id);
        index.save(directories).await
    }
}

/// Canonicalizes and records a catalog-backed component.
///
/// The ID must be non-nil; callers satisfy this with validated catalog IDs.
/// Returns an error if canonicalization or index loading or saving fails.
///
/// # Panics
///
/// Panics if `id` is nil.
// TODO: Move UUID validation and path canonicalization into
// `ComponentIndex::record`, then remove this wrapper.
pub(crate) async fn record(
    directories: &Directories,
    id: Uuid,
    version: String,
    path: PathBuf,
    kind: ItemKind,
) -> Result<()> {
    ComponentIndex::record(
        directories,
        IndexedComponent {
            id: NonNilUuid::new(id).expect("catalog UUID is non-nil"),
            version,
            path: async_fs::canonicalize(path).await?,
            kind,
        },
    )
    .await
}

/// Removes only the identity record, not the component directory.
///
/// A later scan can rediscover the directory under a new UUID. Returns an error
/// if the index cannot be loaded or saved.
// TODO: Expose `ComponentIndex::remove` within the addons module, then remove
// this forwarding wrapper.
pub(crate) async fn remove(directories: &Directories, id: Uuid) -> Result<()> {
    ComponentIndex::remove(directories, id).await
}

/// Discovers supported component directories and synchronizes the local index.
///
/// Results are sorted by canonical path. Stale index entries are pruned, new
/// hand-placed components receive random IDs, and existing IDs and version
/// labels are retained only when their canonical paths still match. Any
/// filesystem, index, or recognized-component validation error aborts without
/// returning a partial result.
pub(crate) async fn scan(directories: &Directories) -> Result<Vec<IndexedComponent>> {
    let components_path = async_fs::canonicalize(directories.components()).await?;
    let index = ComponentIndex::load(directories).await?;
    let has_index = index.is_some();
    let index = index.unwrap_or_default();
    let mut components = discover_components(&components_path, &index).await?;
    components.sort_unstable_by(|left, right| left.path.cmp(&right.path));
    let next = ComponentIndex {
        components: components.clone(),
    };
    if next != index || !has_index {
        next.save(directories).await?;
    }
    Ok(components)
}

async fn discover_components(
    components_path: &Path,
    index: &ComponentIndex,
) -> Result<Vec<IndexedComponent>> {
    let mut indexed: HashMap<_, _> = index
        .components
        .iter()
        .map(|entry| (entry.path.clone(), entry))
        .collect();
    let mut components = Vec::new();
    let mut categories = async_fs::read_dir(components_path).await?;
    while let Some(category_entry) = categories.try_next().await? {
        if !category_entry.file_type().await?.is_dir() {
            continue;
        }
        let category = category_entry.file_name().to_string_lossy().into_owned();
        let mut versions = async_fs::read_dir(category_entry.path()).await?;
        while let Some(version_entry) = versions.try_next().await? {
            if !version_entry.file_type().await?.is_dir() {
                continue;
            }
            let path = async_fs::canonicalize(version_entry.path()).await?;
            let Some(kind) = detect_kind(&category, &path).await? else {
                continue;
            };
            let (id, version) = match indexed.remove(&path) {
                Some(entry) => (entry.id, entry.version.clone()),
                None => (
                    NonNilUuid::new(Uuid::new_v4()).expect("v4 UUID is non-nil"),
                    version_entry.file_name().to_string_lossy().into_owned(),
                ),
            };
            components.push(IndexedComponent {
                id,
                version,
                path,
                kind,
            });
        }
    }
    Ok(components)
}

/// Classifies a component directory from its category and required contents.
///
/// Returns `None` for unknown categories. Winebridge and UMU directories must
/// contain `bottles-winebridge.exe` and `umu-run`, respectively. A runner must
/// contain a regular `proton` file or `bin/wine`; Proton takes precedence when
/// both exist. Slot-addon directories require no marker file. Validation and
/// filesystem failures are returned to the caller.
pub(crate) async fn detect_kind(category: &str, path: &Path) -> Result<Option<ItemKind>> {
    Ok(Some(match category {
        "runners" => ItemKind::RunnerComponent {
            flavour: detect_runner_kind(path).await?,
        },
        "winebridge" => {
            if !async_fs::metadata(path.join("bottles-winebridge.exe"))
                .await
                .is_ok_and(|entry| entry.is_file())
            {
                return Err(AddonError::InvalidHandPlacedComponent(path.to_path_buf()).into());
            }
            ItemKind::InternalComponent {
                role: InternalRole::Winebridge,
            }
        }
        "umu" => {
            if !async_fs::metadata(path.join("umu-run"))
                .await
                .is_ok_and(|entry| entry.is_file())
            {
                return Err(AddonError::InvalidHandPlacedComponent(path.to_path_buf()).into());
            }
            ItemKind::InternalComponent {
                role: InternalRole::Umu,
            }
        }
        "dxvk" => ItemKind::Addon {
            slot: Some(Slot::Dxvk),
        },
        "vkd3d" => ItemKind::Addon {
            slot: Some(Slot::Vkd3d),
        },
        "nvapi" => ItemKind::Addon {
            slot: Some(Slot::Nvapi),
        },
        "latency-flex" => ItemKind::Addon {
            slot: Some(Slot::LatencyFlex),
        },
        _ => return Ok(None),
    }))
}
