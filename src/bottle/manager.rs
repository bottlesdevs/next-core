use std::{fs, io, sync::Arc};

use uuid::Uuid;

use crate::{
    Context, Operation,
    bottle::bottle::BottleComponents,
    compatibility::components::Component,
    error::{Error, Result, ResultExt},
    runner::load_runner,
};

use super::{
    FVS_BLOCK_SIZE, PrefixStorage,
    bottle::{Bottle, BottleCache, BottleState, BottleType, DeleteProgress},
    error::BottleError,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CreateProgress {
    Preparing,
    CreatingPrefix,
    InitializingRepository,
}

#[derive(Clone)]
pub struct BottleManager {
    context: Context,
    cache: Arc<BottleCache>,
}

impl BottleManager {
    pub(crate) fn new(context: Context) -> Self {
        Self {
            context,
            cache: Arc::new(Default::default()),
        }
    }

    pub fn create(
        &self,
        name: impl Into<String>,
        kind: BottleType,
        runner: &Component,
        winebridge: &Component,
        umu: Option<&Component>,
    ) -> Operation<Bottle, CreateProgress> {
        let name = name.into();
        let runner = runner.clone();
        let winebridge = winebridge.clone();
        let umu = umu.cloned();
        let cx = self.context.clone();
        let cache = self.cache.clone();
        self.context
            .spawn(move |progress, cancellation| async move {
                progress.send_replace(Some(CreateProgress::Preparing));
                let runner_kind = runner
                    .kind()
                    .runner_kind()
                    .ok_or(BottleError::RunnerComponentRequired)?;
                let components = BottleComponents::new(&runner, &winebridge, umu.as_ref())?;
                let runner = load_runner(
                    runner.path(),
                    runner_kind,
                    umu.as_ref().map(Component::path),
                )?;
                let id = Uuid::new_v4();
                let bottle_path = cx.directories().bottle(id);
                let path = bottle_path.clone();
                cx.spawn_blocking(move || {
                    fs::create_dir_all(path)?;
                    Ok(())
                })
                .await?;

                let result = async {
                    progress.send_replace(Some(CreateProgress::CreatingPrefix));
                    let storage = PrefixStorage::create(
                        kind,
                        &bottle_path,
                        runner.as_ref(),
                        &components.runner().id().to_string(),
                        &cx,
                    )
                    .await?;
                    if cancellation.is_cancelled() {
                        return Err(Error::Cancelled);
                    }

                    let bottle =
                        Bottle::new(id, name, components, Vec::new(), storage, cx.clone()).await?;
                    progress.send_replace(Some(CreateProgress::InitializingRepository));
                    cx.fvs()
                        .await?
                        .new_repository(&bottle_path, FVS_BLOCK_SIZE)
                        .await?;
                    if cancellation.is_cancelled() {
                        return Err(Error::Cancelled);
                    }
                    Ok(Self::intern(&cache, bottle).await)
                }
                .await;

                if result.is_err() {
                    let _ = cx
                        .spawn_blocking(move || {
                            fs::remove_dir_all(bottle_path)?;
                            Ok(())
                        })
                        .await;
                }
                result
            })
    }

    pub async fn open(&self, id: Uuid) -> Result<Bottle> {
        if let Some(bottle) = self.cached(id).await {
            return Ok(bottle);
        }
        let path = self.context.directories().bottle(id).join("bottle.toml");
        let state = self
            .context
            .spawn_blocking(move || {
                if !path.is_file() {
                    return Err(BottleError::NotFound(id).into());
                }
                let state: BottleState = next_config::load(path)?;
                if state.id != id {
                    return Err(BottleError::IdMismatch {
                        expected: id,
                        actual: state.id,
                    }
                    .into());
                }
                Ok(state)
            })
            .await?;
        Ok(Self::intern(&self.cache, Bottle::from_state(state, self.context.clone())).await)
    }

    pub fn delete(&self, id: Uuid) -> Operation<(), DeleteProgress> {
        let manager = self.clone();
        self.context
            .spawn(move |progress, cancellation| async move {
                let bottle = manager.open(id).await?;
                progress.send_replace(Some(DeleteProgress::Stopping));
                bottle.stop().await?;
                if cancellation.is_cancelled() {
                    return Err(Error::Cancelled);
                }
                progress.send_replace(Some(DeleteProgress::Removing));
                let path = manager.context.directories().bottle(id);
                manager
                    .context
                    .spawn_blocking(move || {
                        fs::remove_dir_all(path)?;
                        Ok(())
                    })
                    .await?;
                bottle.mark_deleted();
                manager.cache.lock().await.remove(&id);
                Ok(())
            })
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
                        let Some(config) = next_config::load::<BottleState>(path).log_error()
                        else {
                            continue;
                        };
                        configs.push(config);
                    }
                }
                Ok(configs)
            })
            .await?;
        let mut bottles = Vec::with_capacity(configs.len());
        for config in configs {
            bottles.push(
                Self::intern(
                    &self.cache,
                    Bottle::from_state(config, self.context.clone()),
                )
                .await,
            );
        }
        Ok(bottles)
    }

    async fn cached(&self, id: Uuid) -> Option<Bottle> {
        let mut cache = self.cache.lock().await;
        if let Some(inner) = cache.get(&id).and_then(std::sync::Weak::upgrade) {
            let bottle = Bottle::from_inner(inner);
            if !bottle.is_deleted() {
                return Some(bottle);
            }
        }
        cache.remove(&id);
        None
    }

    async fn intern(cache: &Arc<BottleCache>, bottle: Bottle) -> Bottle {
        let mut entries = cache.lock().await;
        if let Some(inner) = entries.get(&bottle.0.id).and_then(std::sync::Weak::upgrade) {
            let existing = Bottle::from_inner(inner);
            if !existing.is_deleted() {
                return existing;
            }
        }
        entries.insert(bottle.0.id, Arc::downgrade(&bottle.0));
        bottle
    }
}
