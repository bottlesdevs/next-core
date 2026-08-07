use std::{collections::HashMap, io, sync::Arc};

use async_fs as fs;
use futures_core::Stream;
use futures_lite::StreamExt;
use tokio::sync::watch;
use tokio_stream::wrappers::WatchStream;
use uuid::Uuid;

use crate::{
    Context, Operation, Progress, Stage,
    addons::RunnerComponent,
    error::{Error, Result},
    prefix::{FVS_BLOCK_SIZE, Prefix},
};

use super::{
    error::BottleError,
    state::{Bottle, BottleState, BottleType},
};

struct BottleRegistry(watch::Sender<Arc<HashMap<Uuid, Bottle>>>);

fn sorted_bottles(bottles: &HashMap<Uuid, Bottle>) -> Vec<Bottle> {
    let mut bottles = bottles.values().cloned().collect::<Vec<_>>();
    bottles.sort_unstable_by_key(|bottle| bottle.0.id);
    bottles
}

impl BottleRegistry {
    fn new() -> Self {
        let (published, _) = watch::channel(Arc::new(HashMap::new()));
        Self(published)
    }

    fn list(&self) -> Vec<Bottle> {
        sorted_bottles(&self.0.borrow())
    }

    fn get(&self, id: Uuid) -> Option<Bottle> {
        self.0.borrow().get(&id).cloned()
    }

    fn replace(&self, bottles: Vec<Bottle>) {
        self.0.send_replace(Arc::new(
            bottles
                .into_iter()
                .map(|bottle| (bottle.0.id, bottle))
                .collect(),
        ));
    }

    fn intern(&self, bottle: Bottle) -> Bottle {
        let mut interned = bottle.clone();
        self.0.send_if_modified(|published| {
            if let Some(current) = published.get(&bottle.0.id) {
                interned = current.clone();
                return false;
            }
            let mut bottles = published.as_ref().clone();
            bottles.insert(bottle.0.id, bottle);
            *published = Arc::new(bottles);
            true
        });
        interned
    }

    fn remove(&self, id: Uuid) {
        self.0.send_if_modified(|published| {
            let mut bottles = published.as_ref().clone();
            if bottles.remove(&id).is_none() {
                return false;
            }
            *published = Arc::new(bottles);
            true
        });
    }
}

#[derive(Clone)]
pub struct BottleManager {
    pub(super) context: Context,
    registry: Arc<BottleRegistry>,
}

impl BottleManager {
    pub(crate) fn new(context: Context) -> Self {
        Self {
            context,
            registry: Arc::new(BottleRegistry::new()),
        }
    }

    pub(crate) async fn load(context: Context) -> Result<Self> {
        let manager = Self::new(context);
        let bottles = manager.load_bottles().await?;
        manager.registry.replace(bottles);
        Ok(manager)
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
        let registry = self.registry.clone();
        Operation::new(move |progress, cancellation| async move {
            progress.send_replace(Some(Progress::new(Stage::Preparing)));
            let winebridge = cx.addons().winebridge()?;
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
                let bottle = registry.intern(bottle);
                Ok(bottle)
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
            let _write = bottle.0.write_lock.write().await;
            let state = bottle.state()?;
            progress.send_replace(Some(Progress::new(Stage::Stopping)));
            Bottle::stop_state(&state, &bottle.0.cx).await?;
            if cancellation.is_cancelled() {
                return Err(Error::Cancelled);
            }
            progress.send_replace(Some(Progress::new(Stage::Removing)));
            let path = manager.context.directories().bottle(id);
            fs::remove_dir_all(path).await?;
            manager.registry.remove(id);
            bottle.mark_deleted();
            Ok(())
        })
    }

    pub async fn open(&self, id: Uuid) -> Result<Bottle> {
        if let Some(bottle) = self.registry.get(id) {
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
        Ok(self
            .registry
            .intern(Bottle::from_state(state, self.context.clone())))
    }

    pub fn list(&self) -> Vec<Bottle> {
        self.registry.list()
    }

    pub fn watch(&self) -> impl Stream<Item = Vec<Bottle>> + Send + 'static {
        WatchStream::new(self.registry.0.subscribe()).map(|bottles| sorted_bottles(&bottles))
    }

    async fn load_bottles(&self) -> Result<Vec<Bottle>> {
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
        let mut bottles = Vec::with_capacity(paths.len());
        for path in paths {
            match next_config::load::<BottleState>(path).await {
                Ok(state) => bottles.push(Bottle::from_state(state, self.context.clone())),
                Err(error) => tracing::warn!("skipping unreadable bottle: {error}"),
            }
        }
        Ok(bottles)
    }
}
