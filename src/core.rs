use std::{path::PathBuf, sync::Arc};

use crate::{BottleManager, ComponentManager, Context, DependencyManager, Paths, error::Result};

#[derive(Clone)]
pub struct Core {
    _context: Context,
    bottles: BottleManager,
    components: Arc<ComponentManager>,
    dependencies: Arc<DependencyManager>,
}

impl Core {
    pub async fn open(paths: Paths, fvs2d: impl Into<PathBuf>) -> Result<Self> {
        let context = Context::new(paths, fvs2d)?;
        Ok(Self {
            bottles: BottleManager::new(context.clone()),
            components: Arc::new(ComponentManager::load(context.clone()).await?),
            dependencies: Arc::new(DependencyManager::load(context.clone()).await?),
            _context: context,
        })
    }

    pub fn bottles(&self) -> &BottleManager {
        &self.bottles
    }

    pub fn components(&self) -> &ComponentManager {
        &self.components
    }

    pub fn dependencies(&self) -> &DependencyManager {
        &self.dependencies
    }
}
