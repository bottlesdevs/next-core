//! Persisted bottle state and the shared bottle handle.

use std::{
    collections::HashMap,
    ops::AsyncFnOnce,
    path::PathBuf,
    sync::{Arc, Weak},
};

use next_config::Config;
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, watch};
use uuid::Uuid;

use super::{edit::BottleEdit, error::BottleError};
use crate::{
    Context,
    compatibility::{
        components::{Component, catalog::ComponentKind},
        dependencies::Dependency,
    },
    error::Result,
    prefix::Prefix,
    runner::RunnerKind,
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

    pub(crate) runner: RunnerSelection,
    pub(crate) winebridge: Component,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) dxvk: Option<Component>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) vkd3d: Option<Component>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) nvapi: Option<Component>,
    #[serde(
        default,
        rename = "latency-flex",
        skip_serializing_if = "Option::is_none"
    )]
    pub(crate) latency_flex: Option<Component>,
    #[serde(default)]
    pub(crate) dependencies: Vec<Dependency>,
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

    pub fn runner(&self) -> &RunnerSelection {
        &self.runner
    }

    pub fn winebridge(&self) -> &Component {
        &self.winebridge
    }

    pub fn dxvk(&self) -> Option<&Component> {
        self.dxvk.as_ref()
    }

    pub fn vkd3d(&self) -> Option<&Component> {
        self.vkd3d.as_ref()
    }

    pub fn nvapi(&self) -> Option<&Component> {
        self.nvapi.as_ref()
    }

    pub fn latency_flex(&self) -> Option<&Component> {
        self.latency_flex.as_ref()
    }

    pub fn dependencies(&self) -> &[Dependency] {
        &self.dependencies
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

    pub fn kind(&self) -> BottleType {
        self.storage.kind()
    }
}

pub(crate) type BottleCache = Mutex<HashMap<Uuid, Weak<BottleInner>>>;

pub(crate) struct BottleInner {
    pub(crate) published: watch::Sender<Option<Arc<BottleState>>>,
    pub(crate) write: Mutex<()>,
    pub(crate) id: Uuid,
    pub(crate) cx: Context,
}

#[derive(Clone)]
pub struct Bottle(pub(crate) Arc<BottleInner>);

impl Bottle {
    pub(crate) async fn new(
        id: Uuid,
        name: String,
        runner: RunnerSelection,
        winebridge: Component,
        storage: Prefix,
        context: Context,
    ) -> Result<Self> {
        let bottle = Self::from_state(
            BottleState {
                id,
                name,
                runner,
                winebridge,
                dxvk: None,
                vkd3d: None,
                nvapi: None,
                latency_flex: None,
                dependencies: Vec::new(),
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
        Self::from_inner(Arc::new(BottleInner {
            id,
            published,
            write: Mutex::new(()),
            cx,
        }))
    }

    pub(crate) fn from_inner(inner: Arc<BottleInner>) -> Self {
        Self(inner)
    }

    pub fn state(&self) -> Result<Arc<BottleState>> {
        self.0
            .published
            .borrow()
            .clone()
            .ok_or_else(|| BottleError::Deleted(self.0.id).into())
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
        let _write = self.0.write.lock().await;
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
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum RunnerSelection {
    Wine { runner: Component },
    Proton { runner: Component, umu: Component },
}

impl RunnerSelection {
    pub fn wine(runner: Component) -> Result<Self> {
        let selection = Self::Wine { runner };
        selection.validate()?;
        Ok(selection)
    }

    pub fn proton(runner: Component, umu: Component) -> Result<Self> {
        let selection = Self::Proton { runner, umu };
        selection.validate()?;
        Ok(selection)
    }

    pub fn runner(&self) -> &Component {
        match self {
            Self::Wine { runner } | Self::Proton { runner, .. } => runner,
        }
    }

    pub fn umu(&self) -> Option<&Component> {
        match self {
            Self::Wine { .. } => None,
            Self::Proton { umu, .. } => Some(umu),
        }
    }

    pub fn kind(&self) -> RunnerKind {
        match self {
            Self::Wine { .. } => RunnerKind::Wine,
            Self::Proton { .. } => RunnerKind::Proton,
        }
    }

    pub(super) fn validate(&self) -> Result<()> {
        if self.runner().kind().runner_kind() != Some(self.kind()) {
            return Err(BottleError::RunnerComponentRequired.into());
        }
        if self
            .umu()
            .is_some_and(|component| component.kind() != ComponentKind::Umu)
        {
            return Err(BottleError::InvalidUmuComponent.into());
        }
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
pub enum BottleType {
    Standard,
    Virgo,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(unix)]
    fn updates_are_serialized_and_failed_updates_are_rolled_back() {
        futures_lite::future::block_on(async {
            use std::{fs, os::unix::fs::PermissionsExt};

            let root = std::env::temp_dir().join(format!("bottles-next-{}", Uuid::new_v4()));
            let runner_root = root.join("runner");
            fs::create_dir_all(runner_root.join("bin")).unwrap();
            for executable in ["wine", "wineserver"] {
                let path = runner_root.join("bin").join(executable);
                fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
                fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
            }
            let context = Context::for_test(
                crate::Directories::from_path(root.join("data")).unwrap(),
                Some(root.join("fvs2d")),
            )
            .await
            .unwrap();
            let id = Uuid::new_v4();
            fs::create_dir_all(context.directories().bottle(id)).unwrap();
            let runner = Component::new(
                ComponentKind::Runner {
                    kind: RunnerKind::Wine,
                },
                "wine",
                &runner_root,
            )
            .unwrap();
            let winebridge =
                Component::new(ComponentKind::Winebridge, "bridge", "/bridge").unwrap();
            let bottle = Bottle::from_state(
                BottleState {
                    id,
                    name: "test".into(),
                    storage: Prefix::Standard,
                    programs: Vec::new(),
                    wrappers: Wrappers::default(),
                    runner: RunnerSelection::wine(runner.clone()).unwrap(),
                    winebridge,
                    dxvk: None,
                    vkd3d: None,
                    nvapi: None,
                    latency_flex: None,
                    dependencies: Vec::new(),
                    environment: Environment::default(),
                },
                context,
            );
            bottle.save().await.unwrap();

            let result = bottle
                .update(async |state, _| {
                    state.storage = Prefix::Virgo { layers: Vec::new() };
                    state.environment.insert("CHANGED".into(), "yes".into());
                    Err::<(), _>(BottleError::InvalidProgram.into())
                })
                .await;

            assert!(result.is_err());
            let state = bottle.state().unwrap();
            assert!(matches!(state.storage, Prefix::Standard));
            assert!(state.environment.is_empty());

            let first_bottle = bottle.clone();
            let first = async move {
                first_bottle
                    .update(async |state, _| {
                        state.environment.insert("FIRST".into(), "yes".into());
                        futures_lite::future::yield_now().await;
                        Ok(())
                    })
                    .await
            };
            let second_bottle = bottle.clone();
            let second = async move {
                second_bottle
                    .update(async |state, _| {
                        state.environment.insert("SECOND".into(), "yes".into());
                        Ok(())
                    })
                    .await
            };
            let (first, second) = futures_util::future::join(first, second).await;
            first.unwrap();
            second.unwrap();

            let state = bottle.state().unwrap();
            assert_eq!(state.environment.get("FIRST"), Some("yes"));
            assert_eq!(state.environment.get("SECOND"), Some("yes"));
            fs::remove_dir_all(root).unwrap();
        });
    }

    #[test]
    fn runner_components_require_set_runner() {
        futures_lite::future::block_on(async {
            let root = std::env::temp_dir().join(format!("bottles-next-{}", Uuid::new_v4()));
            let context = Context::for_test(
                crate::Directories::from_path(root.join("data")).unwrap(),
                Some(root.join("fvs2d")),
            )
            .await
            .unwrap();
            let wine = Component::new(
                ComponentKind::Runner {
                    kind: RunnerKind::Wine,
                },
                "wine",
                root.join("wine"),
            )
            .unwrap();
            let proton = Component::new(
                ComponentKind::Runner {
                    kind: RunnerKind::Proton,
                },
                "proton",
                root.join("proton"),
            )
            .unwrap();
            let winebridge =
                Component::new(ComponentKind::Winebridge, "bridge", "/bridge").unwrap();
            let bottle = Bottle::from_state(
                BottleState {
                    id: Uuid::new_v4(),
                    name: "test".into(),
                    storage: Prefix::Standard,
                    programs: Vec::new(),
                    wrappers: Wrappers::default(),
                    runner: RunnerSelection::wine(wine.clone()).unwrap(),
                    winebridge,
                    dxvk: None,
                    vkd3d: None,
                    nvapi: None,
                    latency_flex: None,
                    dependencies: Vec::new(),
                    environment: Environment::default(),
                },
                context,
            );

            assert!(matches!(
                bottle.install_component(&proton).await,
                Err(crate::error::Error::Bottle(
                    BottleError::InvalidPrefixComponent
                ))
            ));
        });
    }
}
