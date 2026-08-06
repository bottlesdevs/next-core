use std::{io, sync::Arc};

use async_fs as fs;
use futures_lite::StreamExt;
use uuid::Uuid;

use crate::{
    Context, Operation, Progress, Stage,
    compatibility::RunnerComponent,
    error::{Error, Result},
    prefix::{FVS_BLOCK_SIZE, Prefix},
};

use super::{
    error::BottleError,
    state::{Bottle, BottleCache, BottleState, BottleType},
};

#[derive(Clone)]
pub struct BottleManager {
    pub(super) context: Context,
    pub(super) cache: Arc<BottleCache>,
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
        runner: &RunnerComponent,
    ) -> Operation<Bottle> {
        let name = name.into();
        let runner = runner.clone();
        let cx = self.context.clone();
        let cache = self.cache.clone();
        Operation::new(move |progress, cancellation| async move {
            progress.send_replace(Some(Progress::new(Stage::Preparing)));
            let winebridge = cx.library().winebridge()?;
            let loaded_runner = runner.load().await?;
            let id = Uuid::new_v4();
            let bottle_path = cx.directories().bottle(id);
            fs::create_dir_all(&bottle_path).await?;

            let result = async {
                progress.send_replace(Some(Progress::new(Stage::CreatingPrefix)));
                let storage = Prefix::create(
                    kind,
                    &bottle_path,
                    loaded_runner.as_ref(),
                    &runner.id().to_string(),
                    &cx,
                )
                .await?;
                if cancellation.is_cancelled() {
                    return Err(Error::Cancelled);
                }

                let bottle = Bottle::new(id, name, runner, winebridge, storage, cx.clone()).await?;
                progress.send_replace(Some(Progress::new(Stage::Configuring)));
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
                let _ = fs::remove_dir_all(bottle_path).await;
            }
            result
        })
    }

    pub fn delete(&self, id: Uuid) -> Operation<()> {
        let manager = self.clone();
        Operation::new(move |progress, cancellation| async move {
            let bottle = manager.open(id).await?;
            progress.send_replace(Some(Progress::new(Stage::Stopping)));
            bottle.stop().await?;
            if cancellation.is_cancelled() {
                return Err(Error::Cancelled);
            }
            progress.send_replace(Some(Progress::new(Stage::Removing)));
            let path = manager.context.directories().bottle(id);
            fs::remove_dir_all(path).await?;
            bottle.mark_deleted();
            manager.cache.lock().await.remove(&id);
            Ok(())
        })
    }

    pub async fn open(&self, id: Uuid) -> Result<Bottle> {
        if let Some(bottle) = self.cached(id).await {
            return Ok(bottle);
        }
        let path = self.context.directories().bottle(id).join("bottle.toml");
        if !fs::metadata(&path).await.is_ok_and(|entry| entry.is_file()) {
            return Err(BottleError::NotFound(id).into());
        }
        let state: BottleState = next_config::load(path).await?;
        if state.id != id {
            return Err(BottleError::IdMismatch {
                expected: id,
                actual: state.id,
            }
            .into());
        }
        Ok(Self::intern(&self.cache, Bottle::from_state(state, self.context.clone())).await)
    }

    pub async fn list(&self) -> Result<Vec<Result<Bottle>>> {
        let bottles_path = self.context.directories().bottles();
        let mut entries = match fs::read_dir(bottles_path).await {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        let mut paths = Vec::new();
        while let Some(entry) = entries.try_next().await? {
            let path = entry.path().join("bottle.toml");
            if fs::metadata(&path).await.is_ok_and(|entry| entry.is_file()) {
                paths.push(path);
            }
        }
        let mut configs = Vec::with_capacity(paths.len());
        for path in paths {
            configs.push(
                next_config::load::<BottleState>(path)
                    .await
                    .map_err(Error::from),
            );
        }
        let mut bottles = Vec::with_capacity(configs.len());
        for config in configs {
            bottles.push(match config {
                Ok(config) => Ok(Self::intern(
                    &self.cache,
                    Bottle::from_state(config, self.context.clone()),
                )
                .await),
                Err(error) => Err(error),
            });
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
