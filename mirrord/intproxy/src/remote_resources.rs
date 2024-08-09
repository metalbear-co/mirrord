use std::{
    collections::{hash_map::Entry, HashMap},
    hash::Hash,
};

use mirrord_intproxy_protocol::LayerId;
use tracing::Level;

use crate::proxies::simple::FileResource;

/// For tracking remote resources allocated in the agent: open files and directories, port
/// subscriptions. Remote resources can be shared by multiple layer instances because of forks.
pub struct RemoteResources<T, Resource = ()> {
    by_layer: HashMap<LayerId, HashMap<T, Resource>>,
    counts: HashMap<T, usize>,
}

impl<T, Resource> Default for RemoteResources<T, Resource> {
    fn default() -> Self {
        Self {
            by_layer: Default::default(),
            counts: Default::default(),
        }
    }
}

impl<T, Resource> RemoteResources<T, Resource>
where
    T: Clone + PartialEq + Eq + Hash,
    Resource: Clone,
{
    /// Removes the given resource from the layer instance with the given [`LayerId`].
    /// Returns whether the resource should be closed on the agent side.
    ///
    /// Can be used when the layer closes the resource, e.g. with
    /// [`CloseFileRequest`](mirrord_protocol::file::CloseFileRequest).
    #[tracing::instrument(level = Level::INFO, skip(self, resource), ret)]
    pub(crate) fn remove(&mut self, layer_id: LayerId, resource: T) -> bool {
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

        if removed.is_none() {
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
    #[tracing::instrument(level = Level::INFO, skip(self))]
    pub(crate) fn clone_all(&mut self, src: LayerId, dst: LayerId) {
        let Some(resources) = self.by_layer.get(&src).cloned() else {
            return;
        };

        for resource in resources.keys().cloned() {
            *self.counts.entry(resource).or_default() += 1;
        }

        self.by_layer.insert(dst, resources);
    }

    /// Removes all resources held by the given layer instance.
    /// Returns an [`Iterator`] over resources that should be closed on the agent size.
    ///
    /// Can be used when the layer closes the connection.
    #[tracing::instrument(level = Level::INFO, skip(self))]
    pub(crate) fn remove_all(&mut self, layer_id: LayerId) -> impl '_ + Iterator<Item = T> {
        let resources = self.by_layer.remove(&layer_id).unwrap_or_default();

        resources
            .into_keys()
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

impl<T> RemoteResources<T, ()>
where
    T: Clone + PartialEq + Eq + Hash,
{
    /// Adds the given resource to the layer instance with the given [`LayerId`].
    ///
    /// Used when the layer opens a resource, e.g. with
    /// [`OpenFileRequest`](mirrord_protocol::file::OpenFileRequest).
    #[tracing::instrument(level = Level::INFO, skip(self, resource))]
    pub(crate) fn add(&mut self, layer_id: LayerId, resource: T) {
        let added = self
            .by_layer
            .entry(layer_id)
            .or_default()
            .insert(resource.clone(), ());

        if added.is_some() {
            *self.counts.entry(resource).or_default() += 1;
        }
    }
}

impl<T> RemoteResources<T, FileResource>
where
    T: Clone + PartialEq + Eq + Hash,
{
    /// Adds the given resource to the layer instance with the given [`LayerId`].
    ///
    /// Used when the layer opens a resource, e.g. with
    /// [`OpenFileRequest`](mirrord_protocol::file::OpenFileRequest).
    #[tracing::instrument(level = Level::INFO, skip(self, resource, file))]
    pub(crate) fn add(&mut self, layer_id: LayerId, resource: T, file: FileResource) {
        let added = self
            .by_layer
            .entry(layer_id)
            .or_default()
            .insert(resource.clone(), file);

        if added.is_none() {
            *self.counts.entry(resource).or_default() += 1;
        }
    }

    #[tracing::instrument(level = Level::INFO, skip(self, resource_key))]
    pub(crate) fn get_mut(
        &mut self,
        layer_id: &LayerId,
        resource_key: &T,
    ) -> Option<&mut FileResource> {
        self.by_layer
            .get_mut(layer_id)
            .and_then(|files| files.get_mut(resource_key))
    }
}
