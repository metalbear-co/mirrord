use std::{
    collections::{hash_map::Entry, HashMap, HashSet},
    hash::Hash,
};

use mirrord_intproxy_protocol::LayerId;

/// For tracking remote resources allocated in the agent: open files and directories, port
/// subscriptions. Remote resources can be shared by multiple layer instances because of forks.
pub struct RemoteResources<T> {
    by_layer: HashMap<LayerId, HashSet<T>>,
    counts: HashMap<T, usize>,
}

impl<T> Default for RemoteResources<T> {
    fn default() -> Self {
        Self {
            by_layer: Default::default(),
            counts: Default::default(),
        }
    }
}

impl<T> RemoteResources<T>
where
    T: Clone + PartialEq + Eq + Hash,
{
    /// Adds the given resource to the layer instance with the given [`LayerId`].
    ///
    /// Used when the layer opens a resource, e.g. with
    /// [`OpenFileRequest`](mirrord_protocol::file::OpenFileRequest).
    pub fn add(&mut self, layer_id: LayerId, resource: T) {
        let added = self.by_layer
            .entry(layer_id)
            .or_default()
            .insert(resource.clone());

        if added {
            *self.counts.entry(resource).or_default() += 1;
        }
    }

    /// Removes the given resource from the layer instance with the given [`LayerId`].
    /// Returns whether the resource should be closed on the agent side.
    ///
    /// Can be used when the layer closes the resource, e.g. with
    /// [`CloseFileRequest`](mirrord_protocol::file::CloseFileRequest).
    pub fn remove(&mut self, layer_id: LayerId, resource: T) -> bool {
        let removed = match self.by_layer.entry(layer_id) {
            Entry::Occupied(mut e) => {
                let removed = e.get_mut().remove(&resource);
                if e.get().is_empty() {
                    e.remove();
                }
                removed
            }
            Entry::Vacant(..) => return false,
        };

        if !removed {
            return false;
        }

        match self.counts.entry(resource) {
            Entry::Occupied(e) if *e.get() == 1 => {
                e.remove();
                true
            }
            Entry::Occupied(mut e) => {
                *e.get_mut() -= 1;
                false
            }
            Entry::Vacant(..) => panic!("RemoteResources out of sync"),
        }
    }

    /// Clones all resources held by the layer instance with id `src` to the layer instance with the
    /// id `dst`.
    ///
    /// Can be used when the layer forks.
    pub fn clone_all(&mut self, src: LayerId, dst: LayerId) {
        let Some(resources) = self.by_layer.get(&src).cloned() else {
            return;
        };

        for resource in resources.iter().cloned() {
            *self.counts.entry(resource).or_default() += 1;
        }

        self.by_layer.insert(dst, resources);
    }

    /// Removes all resources held by the given layer instance.
    /// Returns an [`Iterator`] over resources that should be closed on the agent size.
    ///
    /// Can be used when the layer closes the connection.
    pub fn remove_all(&mut self, layer_id: LayerId) -> impl '_ + Iterator<Item = T> {
        let resources = self.by_layer.remove(&layer_id).unwrap_or_default();

        resources
            .into_iter()
            .filter(|resource| match self.counts.entry(resource.clone()) {
                Entry::Occupied(e) if *e.get() == 1 => {
                    e.remove();
                    true
                }
                Entry::Occupied(mut e) => {
                    *e.get_mut() -= 1;
                    false
                }
                Entry::Vacant(..) => panic!("RemoteResources out of sync"),
            })
    }
}
