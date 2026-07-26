use std::{fs, io};

use uuid::Uuid;

use crate::{
    Context,
    bottle::bottle::BottleComponents,
    compatibility::components::Component,
    error::{Result, ResultExt},
    runner::load_runner,
};

use super::{
    FVS_BLOCK_SIZE,
    bottle::{Bottle, BottleConfig, BottleType, PrefixStorage},
    error::BottleError,
};

pub struct BottleManager {
    context: Context,
}

impl BottleManager {
    pub fn new(context: Context) -> Self {
        Self { context }
    }

    pub async fn create(
        &self,
        name: impl Into<String>,
        kind: BottleType,
        runner_component: &Component,
        winebridge: &Component,
        umu: Option<&Component>,
    ) -> Result<Bottle> {
        let name = name.into();
        let bottles_path = self.context.directories().bottles();
        let runtime_path = self.context.directories().runtime_dir().to_path_buf();
        let checked_name = name.clone();
        self.context
            .spawn_blocking(move || {
                fs::create_dir_all(&bottles_path)?;
                fs::create_dir_all(runtime_path)?;
                for entry in fs::read_dir(bottles_path)? {
                    let path = entry?.path().join("bottle.toml");
                    if path.is_file()
                        && next_config::load::<BottleConfig>(&path)?.name == checked_name
                    {
                        return Err(BottleError::DuplicateName(checked_name).into());
                    }
                }
                Ok(())
            })
            .await?;

        let runner_kind = runner_component
            .kind()
            .runner_kind()
            .ok_or(BottleError::RunnerComponentRequired)?;
        let components = BottleComponents::new(runner_component, winebridge, umu)?;
        let runner = load_runner(
            runner_component.path(),
            runner_kind,
            umu.map(Component::path),
        )?;
        let id = Uuid::new_v4();
        let bottle_path = self.bottle_path(id);
        let path = bottle_path.clone();
        self.context
            .spawn_blocking(move || {
                fs::create_dir_all(path)?;
                Ok(())
            })
            .await?;

        let result = async {
            let storage = PrefixStorage::create(
                kind,
                &bottle_path,
                runner.as_ref(),
                &runner_component.id().to_string(),
                &self.context,
            )
            .await?;

            let bottle = Bottle::new(
                id,
                name,
                components,
                Vec::new(),
                storage,
                self.context.clone(),
            )
            .await?;
            self.context
                .fvs()
                .await?
                .new_repository(&bottle_path, FVS_BLOCK_SIZE)
                .await?;
            Ok(bottle)
        }
        .await;

        if result.is_err() {
            let _ = self
                .context
                .spawn_blocking(move || {
                    fs::remove_dir_all(bottle_path)?;
                    Ok(())
                })
                .await;
        }
        result
    }

    pub async fn open(&self, id: Uuid) -> Result<Bottle> {
        let path = self.bottle_path(id).join("bottle.toml");
        let config = self
            .context
            .spawn_blocking(move || {
                if !path.is_file() {
                    return Err(BottleError::NotFound(id).into());
                }
                let config: BottleConfig = next_config::load(path)?;
                if config.id != id {
                    return Err(BottleError::IdMismatch {
                        expected: id,
                        actual: config.id,
                    }
                    .into());
                }
                Ok(config)
            })
            .await?;
        Ok(Bottle::from_config(config, self.context.clone()))
    }

    pub async fn list(&self) -> Result<Vec<Bottle>> {
        let bottles_path = self.context.directories().bottles();
        let configs = self
            .context
            .spawn_blocking(move || {
                let entries = match fs::read_dir(bottles_path) {
                    Ok(entries) => entries,
                    Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
                    Err(error) => return Err(error.into()),
                };
                let mut configs = Vec::new();
                for entry in entries {
                    let path = entry?.path().join("bottle.toml");
                    if path.is_file() {
                        let Some(config) = next_config::load::<BottleConfig>(path).log_error()
                        else {
                            continue;
                        };
                        configs.push(config);
                    }
                }
                Ok(configs)
            })
            .await?;
        Ok(configs
            .into_iter()
            .map(|config| Bottle::from_config(config, self.context.clone()))
            .collect())
    }

    pub async fn delete(&self, id: Uuid) -> Result<()> {
        let mut bottle = self.open(id).await?;
        bottle.stop().await?;
        let path = self.bottle_path(id);
        self.context
            .spawn_blocking(move || {
                fs::remove_dir_all(path)?;
                Ok(())
            })
            .await
    }

    fn bottle_path(&self, id: Uuid) -> std::path::PathBuf {
        self.context.directories().bottle(id)
    }
}
