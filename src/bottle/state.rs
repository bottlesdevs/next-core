//! Persisted bottle state and the shared bottle handle.

use std::{ops::AsyncFnOnce, path::PathBuf, sync::Arc};

use futures_core::Stream;
use next_config::Config;
use serde::{Deserialize, Serialize};
use tokio::sync::{RwLock, watch};
use tokio_stream::{StreamExt, wrappers::WatchStream};
use uuid::Uuid;

use super::{edit::BottleEdit, error::BottleError};
use crate::{
    Context,
    addons::{Addon, RunnerComponent, Slot, item::InternalComponent},
    error::Result,
    prefix::Prefix,
    utils::environment::Environment,
    wrapper::Wrappers,
};

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, Config)]
#[config(version = 1)]
pub struct BottleState {
    pub(crate) id: Uuid,
    pub(crate) name: String,
    pub(crate) storage: Prefix,
    #[serde(default)]
    pub(crate) programs: Vec<Program>,

    pub(crate) runner: RunnerComponent,
    pub(crate) winebridge: InternalComponent,
    #[serde(default)]
    pub(crate) addons: Vec<Addon>,
    #[serde(default, skip_serializing_if = "Environment::is_empty")]
    pub(crate) environment: Environment,

    #[serde(flatten)]
    pub(crate) wrappers: Wrappers,
}

impl BottleState {
    pub fn id(&self) -> Uuid {
        self.id
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn runner(&self) -> &RunnerComponent {
        &self.runner
    }

    pub fn addons(&self) -> &[Addon] {
        &self.addons
    }

    pub fn addon(&self, slot: Slot) -> Option<&Addon> {
        self.addons.iter().find(|addon| addon.slot() == Some(slot))
    }

    pub(crate) fn replaced_addon_id(&self, addon: &Addon) -> Option<Uuid> {
        addon
            .slot()
            .and_then(|slot| self.addon(slot))
            .map(Addon::id)
    }

    pub(crate) fn put_addon(&mut self, addon: Addon) {
        if let Some(slot) = addon.slot() {
            self.addons
                .retain(|installed| installed.slot() != Some(slot));
        }
        self.addons.push(addon);
    }

    pub fn environment(&self) -> &Environment {
        &self.environment
    }

    pub fn wrappers(&self) -> &Wrappers {
        &self.wrappers
    }

    pub fn programs(&self) -> &[Program] {
        &self.programs
    }

    pub fn program(&self, id: Uuid) -> Option<&Program> {
        self.programs.iter().find(|program| program.id == id)
    }

    pub fn storage(&self) -> Storage {
        self.storage.kind()
    }
}

pub(crate) struct BottleInner {
    pub(crate) published: watch::Sender<Option<Arc<BottleState>>>,
    pub(crate) write_lock: RwLock<()>,
    pub(crate) id: Uuid,
    pub(crate) cx: Context,
}

#[derive(Clone)]
pub struct Bottle(pub(crate) Arc<BottleInner>);

impl Bottle {
    pub(crate) async fn new(
        id: Uuid,
        name: String,
        runner: RunnerComponent,
        winebridge: InternalComponent,
        storage: Prefix,
        context: Context,
    ) -> Result<Self> {
        let bottle = Self::from_state(
            BottleState {
                id,
                name,
                runner,
                winebridge,
                addons: Vec::new(),
                storage,
                programs: Vec::new(),
                wrappers: Wrappers::default(),
                environment: Environment::default(),
            },
            context,
        );
        bottle.save().await?;
        Ok(bottle)
    }

    pub(crate) fn from_state(state: BottleState, cx: Context) -> Self {
        let id = state.id;
        let (published, _) = watch::channel(Some(Arc::new(state)));
        Self(Arc::new(BottleInner {
            id,
            published,
            write_lock: RwLock::new(()),
            cx,
        }))
    }

    pub fn state(&self) -> Result<Arc<BottleState>> {
        self.0
            .published
            .borrow()
            .clone()
            .ok_or_else(|| BottleError::Deleted(self.0.id).into())
    }

    pub fn watch(&self) -> impl Stream<Item = Arc<BottleState>> + Send + 'static {
        WatchStream::new(self.0.published.subscribe())
            .take_while(Option::is_some)
            .filter_map(|state| state)
    }

    pub fn edit(&self) -> BottleEdit {
        BottleEdit::new(self.clone())
    }

    pub(crate) fn ensure_exists(&self) -> Result<()> {
        if self.is_deleted() {
            Err(BottleError::Deleted(self.0.id).into())
        } else {
            Ok(())
        }
    }

    pub(crate) fn is_deleted(&self) -> bool {
        self.0.published.borrow().is_none()
    }

    pub(crate) fn mark_deleted(&self) {
        self.0.published.send_replace(None);
    }

    pub(super) async fn update<F, R>(&self, operation: F) -> Result<R>
    where
        F: for<'a> AsyncFnOnce(&'a mut BottleState, Context) -> Result<R>,
    {
        let _write = self.0.write_lock.write().await;
        let mut draft = self.state()?.as_ref().clone();
        let value = operation(&mut draft, self.0.cx.clone()).await?;
        Self::save_state(&draft, &self.0.cx).await?;
        self.publish(draft);
        Ok(value)
    }

    pub(crate) fn publish(&self, state: BottleState) {
        let next = Arc::new(state);
        self.0.published.send_if_modified(|published| {
            if published.as_deref() == Some(next.as_ref()) {
                false
            } else {
                *published = Some(next);
                true
            }
        });
    }

    pub(crate) fn bottle_path(&self) -> PathBuf {
        self.0.cx.directories().bottle(self.0.id)
    }

    pub(crate) fn prefix_path(&self) -> PathBuf {
        self.bottle_path().join("prefix")
    }

    async fn save(&self) -> Result<()> {
        let state = self.state()?;
        Self::save_state(&state, &self.0.cx).await
    }

    async fn save_state(state: &BottleState, cx: &Context) -> Result<()> {
        let path = cx.directories().bottle(state.id).join("bottle.toml");
        next_config::save(path, state).await?;
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct Program {
    pub id: Uuid,
    pub name: String,
    pub executable: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub working_directory: Option<String>,
    #[serde(default)]
    pub new_console: bool,
}

impl Program {
    pub fn new(name: impl Into<String>, executable: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: name.into(),
            executable: executable.into(),
            args: Vec::new(),
            working_directory: None,
            new_console: false,
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
pub enum Storage {
    Standard,
    Virgo,
}
