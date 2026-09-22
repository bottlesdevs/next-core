const MANIFEST_FILE: &str = "plugin.toml";
const COMPONENT_FILE: &str = "plugin.wasm";

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
};

use bottles_plugin_host::{CompiledPlugin, Runtime};
use futures_lite::StreamExt;
use tokio::sync::Mutex;

use crate::{Directories, utils::storage};

pub use bottles_plugin_host::{PluginError, PluginInfo, PluginManifest};

type Result<T> = std::result::Result<T, PluginError>;

/// External package lifecycle. Package loading failures abort startup.
pub struct Plugins {
    directories: Directories,
    runtime: Runtime,
    lifecycle: Mutex<()>,
    installed: RwLock<HashMap<String, InstalledPlugin>>,
}

impl Plugins {
    pub(crate) async fn open(directories: &Directories) -> Result<Self> {
        let installed_directory = directories.plugins().join("installed");
        async_fs::create_dir_all(&installed_directory).await?;
        let runtime = Runtime::new().map_err(PluginError::Runtime)?;
        let installed = discover(&installed_directory).await?;
        Ok(Self {
            directories: directories.clone(),
            runtime,
            lifecycle: Mutex::new(()),
            installed: RwLock::new(installed),
        })
    }

    pub fn list(&self) -> Vec<PluginInfo> {
        self.installed
            .read()
            .unwrap()
            .values()
            .map(|plugin| plugin.info.clone())
            .collect()
    }

    pub async fn install(&self, directory: &Path) -> Result<PluginInfo> {
        let _lifecycle = self.lifecycle.lock().await;
        let info = read_info(directory).await?;
        copy_package(directory, &self.package_directory(&info.manifest.id)).await?;
        self.installed.write().unwrap().insert(
            info.manifest.id.clone(),
            InstalledPlugin {
                info: info.clone(),
                component: None,
            },
        );
        Ok(info)
    }

    pub async fn reload(&self, id: &String) -> Result<()> {
        let _lifecycle = self.lifecycle.lock().await;
        let info = read_info(&self.package_directory(id)).await?;
        self.installed.write().unwrap().insert(
            info.manifest.id.clone(),
            InstalledPlugin {
                info,
                component: None,
            },
        );
        Ok(())
    }

    /// Withdraw the package before unloading it; trash cleanup cannot fail the removal.
    pub async fn uninstall(&self, id: &String) -> Result<()> {
        let _lifecycle = self.lifecycle.lock().await;
        storage::with_temp_dir(&self.directories.trash(), |trash| async move {
            async_fs::rename(self.package_directory(id), trash.join("plugin")).await?;
            self.installed.write().unwrap().remove(id);
            Ok(())
        })
        .await
    }

    pub(crate) fn get(&self, id: &str) -> Option<PluginInfo> {
        self.installed
            .read()
            .unwrap()
            .get(id)
            .map(|plugin| plugin.info.clone())
    }

    pub(crate) async fn load(&self, id: &String) -> Result<Plugin> {
        let _lifecycle = self.lifecycle.lock().await;
        let (info, component) = {
            let installed = self.installed.read().unwrap();
            let plugin = installed
                .get(id)
                .ok_or_else(|| PluginError::NotFound(id.clone()))?;
            (plugin.info.clone(), plugin.component.clone())
        };
        let component = match component {
            Some(component) => component,
            None => {
                let bytes = async_fs::read(self.package_directory(id).join(COMPONENT_FILE)).await?;
                let component = Arc::new(
                    self.runtime
                        .compile(bytes)
                        .await
                        .map_err(PluginError::Runtime)?,
                );
                // The lifecycle guard keeps this entry installed until compilation finishes.
                self.installed
                    .write()
                    .unwrap()
                    .get_mut(id)
                    .unwrap()
                    .component = Some(component.clone());
                component
            }
        };
        Ok(Plugin {
            manifest: info.manifest,
            component,
        })
    }

    fn package_directory(&self, id: &String) -> PathBuf {
        self.directories
            .plugins()
            .join("installed")
            .join(id.as_str())
    }
}

struct InstalledPlugin {
    info: PluginInfo,
    component: Option<Arc<CompiledPlugin>>,
}

async fn discover(directory: &Path) -> Result<HashMap<String, InstalledPlugin>> {
    let mut directories = async_fs::read_dir(directory).await?;
    let mut installed = HashMap::new();
    while let Some(entry) = directories.next().await.transpose()? {
        if !entry.file_type().await?.is_dir() {
            continue;
        }
        let info = read_info(&entry.path()).await?;
        installed.insert(
            info.manifest.id.clone(),
            InstalledPlugin {
                info,
                component: None,
            },
        );
    }
    Ok(installed)
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
    pub(crate) component: Arc<CompiledPlugin>,
}

async fn read_info(directory: &Path) -> Result<PluginInfo> {
    let manifest = bottles_plugin_host::parse_manifest(
        &async_fs::read_to_string(directory.join(MANIFEST_FILE)).await?,
    )?;
    let interfaces = bottles_plugin_host::exported_interfaces(
        &async_fs::read(directory.join(COMPONENT_FILE)).await?,
    )?;
    Ok(PluginInfo {
        manifest,
        interfaces,
    })
}
