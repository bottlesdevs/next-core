use std::{fs, path::PathBuf, sync::OnceLock};

use fvs_rs::Fvs2dClient;
use tokio::sync::OnceCell;
use uuid::Uuid;

use crate::{
    BottleComponents,
    compatibility::{components::Component, installer::umu_for_runner},
    error::Result,
    runner::{initialize_and_shutdown_prefix, load_runner},
    utils::absolute_path,
};

use super::{
    bottle::{Bottle, BottleType, PrefixStorage},
    error::BottleError,
};

pub struct BottleManagerConfig {
    pub fvs2d_executable: Option<PathBuf>,
}

static CONFIG: OnceLock<BottleManagerConfig> = OnceLock::new();
static FVS: OnceCell<Fvs2dClient> = OnceCell::const_new();

pub struct BottleManager;

impl BottleManager {
    pub fn new(config: BottleManagerConfig) -> Result<Self> {
        // TODO: Directories shouldn't be created here
        let directories =
            crate::utils::directories::get().ok_or(BottleError::ProjectDirectoriesUnavailable)?;
        fs::create_dir_all(directories.bottles())?;
        fs::create_dir_all(directories.runtime_dir())?;

        let config = BottleManagerConfig {
            fvs2d_executable: config.fvs2d_executable.map(absolute_path).transpose()?,
        };

        CONFIG.get_or_init(|| config);

        Ok(Self)
    }

    pub async fn create(
        &self,
        name: impl Into<String>,
        kind: BottleType,
        runner_component: &Component,
        winebridge: &Component,
    ) -> Result<Bottle> {
        let name = name.into();
        for entry in fs::read_dir(crate::utils::directories::expect().bottles())? {
            let path = entry?.path().join("bottle.toml");
            if path.is_file() && next_config::load::<Bottle>(&path)?.name == name {
                return Err(BottleError::DuplicateName(name).into());
            }
        }

        let runner_kind = runner_component.kind().runner_kind().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "runner component is required",
            )
        })?;
        let umu = umu_for_runner(runner_kind, None)?;
        let components = BottleComponents::new(runner_component, winebridge, umu.as_ref())?;
        let runner = load_runner(
            runner_component.path(),
            runner_kind,
            umu.as_ref().map(Component::path),
        )?;
        let id = Uuid::new_v4();
        let root = self.bottle_root(id);
        fs::create_dir_all(&root)?;

        let result = async {
            let storage = match kind {
                BottleType::Standard => {
                    let prefix = root.join("prefix");
                    initialize_and_shutdown_prefix(runner.as_ref(), &prefix)?;
                    PrefixStorage::Standard
                }
                BottleType::Virgo => {
                    fs::create_dir_all(root.join("upper"))?;
                    let base = self.ensure_base(runner.as_ref()).await?;
                    let adapter = self
                        .ensure_adapter(runner.as_ref(), &runner_component.id().to_string(), &base)
                        .await?;
                    PrefixStorage::Virgo {
                        layers: vec![base, adapter],
                    }
                }
            };

            Bottle::new(id, name, components, Vec::new(), storage)
        }
        .await;

        if result.is_err() {
            let _ = fs::remove_dir_all(root);
        }
        result
    }

    pub fn open(&self, id: Uuid) -> Result<Bottle> {
        let path = self.bottle_root(id).join("bottle.toml");
        if !path.is_file() {
            return Err(BottleError::NotFound(id).into());
        }
        let bottle: Bottle = next_config::load(path)?;
        if bottle.id != id {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "bottle id does not match its directory",
            )
            .into());
        }
        Ok(bottle)
    }

    pub fn list(&self) -> Result<Vec<Bottle>> {
        let mut bottles: Vec<Bottle> = Vec::new();
        for entry in fs::read_dir(crate::utils::directories::expect().bottles())? {
            let path = entry?.path().join("bottle.toml");
            if path.is_file() {
                bottles.push(next_config::load(path)?);
            }
        }
        Ok(bottles)
    }

    pub async fn delete(&self, id: Uuid) -> Result<()> {
        let mut bottle = self.open(id)?;
        bottle.stop().await?;
        fs::remove_dir_all(self.bottle_root(id))?;
        Ok(())
    }

    fn bottle_root(&self, id: Uuid) -> PathBuf {
        crate::utils::directories::expect().bottle(id)
    }
}

pub(super) fn config() -> &'static BottleManagerConfig {
    CONFIG
        .get()
        .expect("BottleManager initialized runtime configuration")
}

pub(crate) async fn fvs() -> Result<&'static Fvs2dClient> {
    FVS.get_or_try_init(|| async {
        let executable = config()
            .fvs2d_executable
            .as_ref()
            .ok_or(BottleError::VirgoUnavailable)?;
        Ok(Fvs2dClient::connect_or_spawn(
            executable,
            crate::utils::directories::expect()
                .runtime_dir()
                .join("fvs2d.sock"),
        )
        .await?)
    })
    .await
}
