use crate::{
    Directories,
    compatibility::{
        Library,
        components::Component,
        installer::{InstallStep, component_steps},
    },
    error::Result,
    utils::absolute_path,
};
use fvs_rs::Fvs2dClient;
use std::{
    path::PathBuf,
    sync::{Arc, OnceLock},
};
use tokio::sync::OnceCell;

struct ContextInner {
    directories: Directories,
    fvs2d_executable: PathBuf,
    fvs: OnceCell<Fvs2dClient>,
    library: OnceLock<Arc<Library>>,
}

#[derive(Clone)]
pub(crate) struct Context(Arc<ContextInner>);

impl Context {
    pub(crate) fn new(
        directories: Directories,
        fvs2d_executable: impl Into<PathBuf>,
    ) -> Result<Self> {
        Ok(Self(Arc::new(ContextInner {
            directories,
            fvs2d_executable: absolute_path(fvs2d_executable.into())?,
            fvs: OnceCell::new(),
            library: OnceLock::new(),
        })))
    }

    pub(crate) fn directories(&self) -> &Directories {
        &self.0.directories
    }

    pub(crate) fn set_library(&self, library: Arc<Library>) {
        self.0
            .library
            .set(library)
            .unwrap_or_else(|_| panic!("context library already initialized"));
    }

    pub(crate) fn library(&self) -> Option<&Library> {
        self.0.library.get().map(Arc::as_ref)
    }

    pub(crate) fn component_steps(&self, component: &Component) -> Vec<InstallStep> {
        if let Some(library) = self.library() {
            return library.component_steps(component);
        }

        component_steps(component.kind())
            .unwrap_or_default()
            .to_vec()
    }

    pub(crate) async fn fvs(&self) -> Result<&Fvs2dClient> {
        self.0
            .fvs
            .get_or_try_init(|| async {
                Ok(Fvs2dClient::connect_or_spawn(
                    &self.0.fvs2d_executable,
                    self.0.directories.runtime_dir().join("fvs2d.sock"),
                )
                .await?)
            })
            .await
    }
}
