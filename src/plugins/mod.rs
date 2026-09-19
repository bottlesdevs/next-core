mod manifest;

const MANIFEST_FILE: &str = "plugin.toml";
const COMPONENT_FILE: &str = "plugin.wasm";

use std::{
    collections::HashMap,
    io,
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
};

use bottles_plugin_host::Plugin as WasmPlugin;
use futures_lite::StreamExt;
use tokio::sync::Mutex;

use crate::{Directories, utils::storage};

pub use bottles_plugin_host::PluginKind;
pub use manifest::{PluginId, PluginManifest};

#[derive(Debug, thiserror::Error)]
pub enum PluginError {
    #[error("plugin manifest schema {0} is not supported")]
    UnsupportedSchema(u32),
    #[error("failed to parse plugin manifest: {0}")]
    ParseManifest(#[from] toml::de::Error),
    #[error("plugin {0} was not found")]
    NotFound(PluginId),
    #[error("plugin runtime failed: {0}")]
    Runtime(String),
    #[error("I/O: {0}")]
    Io(#[from] io::Error),
}

type Result<T> = std::result::Result<T, PluginError>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginInfo {
    pub manifest: PluginManifest,
    pub provides: Vec<PluginKind>,
}

/// External package lifecycle. Package loading failures abort startup.
pub struct Plugins {
    directories: Directories,
    lifecycle: Mutex<()>,
    loaded: RwLock<HashMap<PluginId, Plugin>>,
}

impl Plugins {
    pub(crate) async fn open(directories: &Directories) -> Result<Self> {
        let installed_directory = directories.plugins().join("installed");
        async_fs::create_dir_all(&installed_directory).await?;
        let loaded = discover(&installed_directory).await?;
        Ok(Self {
            directories: directories.clone(),
            lifecycle: Mutex::new(()),
            loaded: RwLock::new(loaded),
        })
    }

    pub fn list(&self) -> Vec<PluginInfo> {
        self.loaded
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .map(Plugin::info)
            .collect()
    }

    pub async fn install(&self, directory: &Path) -> Result<PluginInfo> {
        let _lifecycle = self.lifecycle.lock().await;
        let plugin = Plugin::load(directory).await?;
        copy_package(directory, &self.package_directory(&plugin.manifest.id)).await?;
        let info = plugin.info();
        self.loaded
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(plugin.manifest.id.clone(), plugin);
        Ok(info)
    }

    pub async fn reload(&self, id: &PluginId) -> Result<()> {
        let _lifecycle = self.lifecycle.lock().await;
        if !self
            .loaded
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains_key(id)
        {
            return Err(PluginError::NotFound(id.clone()));
        }
        let plugin = Plugin::load(&self.package_directory(id)).await?;
        self.loaded
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(plugin.manifest.id.clone(), plugin);
        Ok(())
    }

    /// Withdraw the package before unloading it; trash cleanup cannot fail the removal.
    pub async fn uninstall(&self, id: &PluginId) -> Result<()> {
        let _lifecycle = self.lifecycle.lock().await;
        if self.get(id).is_none() {
            return Err(PluginError::NotFound(id.clone()));
        }
        storage::with_temp_dir(&self.directories.trash(), |trash| async move {
            async_fs::rename(self.package_directory(id), trash.join("plugin")).await?;
            self.loaded
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(id);
            Ok(())
        })
        .await
    }

    pub(crate) fn get(&self, id: &PluginId) -> Option<Plugin> {
        self.loaded
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(id)
            .cloned()
    }

    pub(crate) fn contribution(&self, id: &PluginId, kind: PluginKind) -> Option<Plugin> {
        self.get(id)
            .filter(|plugin| plugin.runtime.provides().contains(&kind))
    }

    pub(crate) fn contributions(&self, kind: PluginKind) -> Vec<Plugin> {
        self.loaded
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .filter_map(|plugin| plugin.contribution(kind))
            .collect()
    }

    fn package_directory(&self, id: &PluginId) -> PathBuf {
        self.directories
            .plugins()
            .join("installed")
            .join(id.as_str())
    }
}

async fn discover(directory: &Path) -> Result<HashMap<PluginId, Plugin>> {
    let mut directories = async_fs::read_dir(directory).await?;
    let mut loaded = HashMap::new();
    while let Some(entry) = directories.next().await.transpose()? {
        if !entry.file_type().await?.is_dir() {
            continue;
        }
        let plugin = Plugin::load(&entry.path()).await?;
        loaded.insert(plugin.manifest.id.clone(), plugin);
    }
    Ok(loaded)
}

async fn copy_package(source: &Path, destination: &Path) -> Result<()> {
    async_fs::create_dir_all(destination).await?;
    for file in [MANIFEST_FILE, COMPONENT_FILE] {
        async_fs::copy(source.join(file), destination.join(file)).await?;
    }
    Ok(())
}

#[derive(Clone)]
pub(crate) struct Plugin {
    pub(crate) manifest: PluginManifest,
    pub(crate) runtime: Arc<WasmPlugin>,
}

impl Plugin {
    async fn load(directory: &Path) -> Result<Self> {
        let manifest = PluginManifest::load(directory).await?;
        let component = async_fs::read(directory.join(COMPONENT_FILE)).await?;
        let runtime = Arc::new(
            WasmPlugin::load(&component)
                .await
                .map_err(PluginError::Runtime)?,
        );
        Ok(Self { manifest, runtime })
    }

    fn info(&self) -> PluginInfo {
        PluginInfo {
            manifest: self.manifest.clone(),
            provides: self.runtime.provides().to_vec(),
        }
    }

    pub(crate) fn contribution(&self, kind: PluginKind) -> Option<Self> {
        self.runtime
            .provides()
            .contains(&kind)
            .then(|| self.clone())
    }
}
