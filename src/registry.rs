use std::collections::HashMap;
use std::sync::Arc;

use next_proto::bottles::common::v1::Storefront;

use crate::plugins::StorePlugin;

#[derive(Default, Clone)]
pub struct StoreRegistry {
    plugins: HashMap<Storefront, Arc<dyn StorePlugin>>,
}

impl StoreRegistry {
    pub fn new(plugins: impl IntoIterator<Item = Arc<dyn StorePlugin>>) -> Self {
        let plugins = plugins
            .into_iter()
            .map(|plugin| (plugin.storefront(), plugin))
            .collect();

        Self { plugins }
    }

    pub fn get(&self, storefront: Storefront) -> Option<&Arc<dyn StorePlugin>> {
        self.plugins.get(&storefront)
    }

    pub fn storefronts(&self) -> impl Iterator<Item = Storefront> + '_ {
        self.plugins.keys().copied()
    }
}
