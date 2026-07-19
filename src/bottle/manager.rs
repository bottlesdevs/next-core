use std::{fs, path::PathBuf, sync::OnceLock};

use fvs_rs::Fvs2dClient;
use tokio::sync::OnceCell;
use uuid::Uuid;

use crate::{
    bottle::bottle::BottleComponents,
    compatibility::{components::Component, installer::umu_for_runner},
    error::Result,
    runner::load_runner,
    utils::absolute_path,
};

use super::{
    bottle::{Bottle, BottleType, PrefixStorage},
    error::{BottleError, VirgoError},
};

pub struct BottleManagerConfig {
    pub fvs2d_executable: Option<PathBuf>,
}

static CONFIG: OnceLock<BottleManagerConfig> = OnceLock::new();
static FVS: OnceCell<Fvs2dClient> = OnceCell::const_new();

pub struct BottleManager;

impl BottleManager {
    pub fn new(fvs2d_executable: Option<PathBuf>) -> Result<Self> {
        // TODO: Directories shouldn't be created here
        let directories =
            crate::utils::directories::get().ok_or(BottleError::ProjectDirectoriesUnavailable)?;
        fs::create_dir_all(directories.bottles())?;
        fs::create_dir_all(directories.runtime_dir())?;

        let config = BottleManagerConfig {
            fvs2d_executable: fvs2d_executable.map(absolute_path).transpose()?,
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

        let runner_kind = runner_component
            .kind()
            .runner_kind()
            .ok_or(BottleError::RunnerComponentRequired)?;
        let umu = umu_for_runner(runner_kind, None)?;
        let components = BottleComponents::new(runner_component, winebridge, umu.as_ref())?;
        let runner = load_runner(
            runner_component.path(),
            runner_kind,
            umu.as_ref().map(Component::path),
        )?;
        let id = Uuid::new_v4();
        let bottle_path = self.bottle_path(id);
        fs::create_dir_all(&bottle_path)?;

        let result = async {
            let storage = PrefixStorage::create(
                kind,
                &bottle_path,
                runner.as_ref(),
                &runner_component.id().to_string(),
            )
            .await?;

            Bottle::new(id, name, components, Vec::new(), storage)
        }
        .await;

        if result.is_err() {
            let _ = fs::remove_dir_all(bottle_path);
        }
        result
    }

    pub fn open(&self, id: Uuid) -> Result<Bottle> {
        let path = self.bottle_path(id).join("bottle.toml");
        if !path.is_file() {
            return Err(BottleError::NotFound(id).into());
        }
        let bottle: Bottle = next_config::load(path)?;
        if bottle.id != id {
            return Err(BottleError::IdMismatch {
                expected: id,
                actual: bottle.id,
            }
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
        fs::remove_dir_all(self.bottle_path(id))?;
        Ok(())
    }

    fn bottle_path(&self, id: Uuid) -> PathBuf {
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
            .ok_or(VirgoError::Unavailable)?;
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
