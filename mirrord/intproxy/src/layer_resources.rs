use std::{
    collections::{HashMap, HashSet},
    hash::Hash,
    sync::Arc,
};

use crate::protocol::LayerId;

pub struct LayerResources<T> {
    layers: HashMap<LayerId, HashSet<Arc<T>>>,
}

impl<T> Default for LayerResources<T> {
    fn default() -> Self {
        Self {
            layers: Default::default(),
        }
    }
}

impl<T> LayerResources<T>
where
    T: Clone + PartialEq + Eq + Hash,
{
    pub fn add(&mut self, layer_id: LayerId, resource: T) {
        self.layers
            .entry(layer_id)
            .or_default()
            .insert(resource.into());
    }

    pub fn remove(&mut self, layer_id: LayerId, resource: T) -> bool {
        let Some(resources) = self.layers.get_mut(&layer_id) else {
            return false;
        };

        resources
            .take(&resource)
            .and_then(Arc::into_inner)
            .is_some()
    }

    pub fn clone_all(&mut self, src: LayerId, dst: LayerId) {
        let resources = self.layers.get(&src).cloned().unwrap_or_default();
        self.layers.insert(dst, resources);
    }

    pub fn remove_all(&mut self, layer_id: LayerId) -> impl Iterator<Item = T> {
        self.layers
            .remove(&layer_id)
            .unwrap_or_default()
            .into_iter()
            .filter_map(Arc::into_inner)
    }
}
