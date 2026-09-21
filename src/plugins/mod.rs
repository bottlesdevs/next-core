const MANIFEST_FILE: &str = "plugin.toml";
const COMPONENT_FILE: &str = "plugin.wasm";

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
};

use bottles_plugin_host::Plugin as WasmPlugin;
use futures_lite::StreamExt;
use tokio::sync::Mutex;

use crate::{Directories, utils::storage};

pub use bottles_plugin_host::{PluginError, PluginInfo, PluginKind, PluginManifest};

type Result<T> = std::result::Result<T, PluginError>;

/// External package lifecycle. Package loading failures abort startup.
pub struct Plugins {
    directories: Directories,
    lifecycle: Mutex<()>,
    loaded: RwLock<HashMap<String, Plugin>>,
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
            .unwrap()
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
            .unwrap()
            .insert(plugin.manifest.id.clone(), plugin);
        Ok(info)
    }

    pub async fn reload(&self, id: &String) -> Result<()> {
        let _lifecycle = self.lifecycle.lock().await;
        let plugin = Plugin::load(&self.package_directory(id)).await?;
        self.loaded
            .write()
            .unwrap()
            .insert(plugin.manifest.id.clone(), plugin);
        Ok(())
    }

    /// Withdraw the package before unloading it; trash cleanup cannot fail the removal.
    pub async fn uninstall(&self, id: &String) -> Result<()> {
        let _lifecycle = self.lifecycle.lock().await;
        storage::with_temp_dir(&self.directories.trash(), |trash| async move {
            async_fs::rename(self.package_directory(id), trash.join("plugin")).await?;
            self.loaded.write().unwrap().remove(id);
            Ok(())
        })
        .await
    }

    pub(crate) fn get(&self, id: &String) -> Option<Plugin> {
        self.loaded.read().unwrap().get(id).cloned()
    }

    pub(crate) fn contribution(&self, id: &String, kind: PluginKind) -> Option<Plugin> {
        self.get(id)
            .filter(|plugin| plugin.runtime.provides().contains(&kind))
    }

    pub(crate) fn contributions(&self, kind: PluginKind) -> Vec<Plugin> {
        self.loaded
            .read()
            .unwrap()
            .values()
            .filter_map(|plugin| plugin.contribution(kind))
            .collect()
    }

    fn package_directory(&self, id: &String) -> PathBuf {
        self.directories
            .plugins()
            .join("installed")
            .join(id.as_str())
    }
}

async fn discover(directory: &Path) -> Result<HashMap<String, Plugin>> {
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
        let manifest = bottles_plugin_host::parse_manifest(
            &async_fs::read_to_string(directory.join(MANIFEST_FILE)).await?,
        )?;
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
