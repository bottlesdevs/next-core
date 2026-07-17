use std::{
    path::{Path, PathBuf},
    sync::LazyLock,
};

#[cfg(not(test))]
use ::directories::ProjectDirs;
use uuid::Uuid;

pub struct Directories {
    data_dir: PathBuf,
    runtime_dir: PathBuf,
}

pub static DIRECTORIES: LazyLock<Option<Directories>> = LazyLock::new(Directories::new);

impl Directories {
    #[cfg(not(test))]
    fn new() -> Option<Self> {
        let project = ProjectDirs::from("com", "usebottles", "bottles-next")?;
        let data_dir = project.data_local_dir().to_path_buf();
        let runtime_dir = project
            .runtime_dir()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| data_dir.join("runtime"));
        Some(Self {
            data_dir,
            runtime_dir,
        })
    }

    #[cfg(test)]
    fn new() -> Option<Self> {
        let root = std::env::temp_dir().join(format!("bottles-next-{}", std::process::id()));
        Some(Self {
            data_dir: root.join("data"),
            runtime_dir: root.join("run"),
        })
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    pub fn runtime_dir(&self) -> &Path {
        &self.runtime_dir
    }

    pub fn bottles(&self) -> PathBuf {
        self.data_dir.join("bottles")
    }

    pub fn bottle(&self, id: Uuid) -> PathBuf {
        self.bottles().join(id.to_string())
    }

    pub fn components(&self) -> PathBuf {
        self.data_dir.join("components")
    }

    pub fn dependencies(&self) -> PathBuf {
        self.data_dir.join("dependencies")
    }
}

pub fn get() -> Option<&'static Directories> {
    DIRECTORIES.as_ref()
}

pub(crate) fn expect() -> &'static Directories {
    get().expect("BottleManager validated application directories")
}
