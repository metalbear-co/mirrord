use std::{
    any::{type_name, TypeId},
    fmt::Debug,
};

use futures::FutureExt;
use futures_util::future::BoxFuture;
use k8s_openapi::{ClusterResourceScope, NamespaceResourceScope};
use kube::{
    api::{ApiResource, DeleteParams, DynamicObject, PostParams},
    Api, Error, Resource,
};
use serde::{de::DeserializeOwned, Serialize};

use crate::utils::kube_client;

/// RAII-style guard for deleting kube resources after tests.
/// This guard deletes the kube resource when dropped.
/// This guard can be configured not to delete the resource if dropped during a panic.
pub struct ResourceGuard {
    /// Whether the resource should be deleted if the test has panicked.
    pub delete_on_fail: bool,
    /// This future will delete the resource once awaited.
    pub deleter: Option<BoxFuture<'static, ()>>,
}

trait ResourceExt: Resource + 'static {
    fn scope() -> ResourceScope {
        match TypeId::of::<Self::Scope>() {
            ty if ty == TypeId::of::<NamespaceResourceScope>() => ResourceScope::Namespaced,
            ty if ty == TypeId::of::<ClusterResourceScope>() => ResourceScope::Cluster,
            _ => ResourceScope::Unsupported(type_name::<Self::Scope>()),
        }
    }
}

impl<K: Resource + 'static> ResourceExt for K {}

#[derive(Debug)]
enum ResourceScope {
    Namespaced,
    Cluster,
    Unsupported(&'static str),
}

impl ResourceGuard {
    /// Create a kube resource and spawn a task to delete it when this guard is dropped.
    /// Return [`Error`] if creating the resource failed.
    pub async fn create<
        K: Resource<DynamicType = ()> + Debug + Clone + DeserializeOwned + Serialize + 'static,
    >(
        api: Api<K>,
        data: &K,
        delete_on_fail: bool,
    ) -> Result<(ResourceGuard, K), Error> {
        let name = data.meta().name.clone().unwrap();
        let namespace = data
            .meta()
            .namespace
            .clone()
            .unwrap_or("default".to_string());

        println!("Creating {} `{name}`: {data:?}", K::kind(&()));
        let created = api.create(&PostParams::default(), data).await?;
        println!("Created {} `{name}`", K::kind(&()));

        let deleter = async move {
            let dyntype = ApiResource::erase::<K>(&());
            // `kube::Client` is a channel + background worker task bound to the runtime it was
            // created on. When `Drop` runs, the test runtime is blocked waiting for the
            // drop to finish, so reusing the original `Api` would deadlock: the deleter
            // needs the worker to advance, but the worker lives on the blocked runtime.
            // Creating a fresh client here gives the deleter its own worker on the new
            // single-threaded runtime, avoiding the deadlock.
            //
            // Deletion only requires the resource name and GVK (group/version/kind), which
            // `ApiResource::erase` captures from `K` at creation time. `DynamicObject`
            // is sufficient here since the deleter doesn't need a typed representation of `K`.
            let client = kube_client().await;
            let api: Api<DynamicObject> = match K::scope() {
                ResourceScope::Cluster => Api::all_with(client, &dyntype),
                ResourceScope::Namespaced => Api::namespaced_with(client, &namespace, &dyntype),
                ResourceScope::Unsupported(ty) => panic!("unsupported resource type: {ty}"),
            };
            println!("Deleting {} `{name}`", K::kind(&()));
            let delete_params = DeleteParams {
                grace_period_seconds: Some(0),
                ..Default::default()
            };
            let res = api.delete(&name, &delete_params).await;
            if let Err(e) = res {
                println!("Failed to delete {} `{name}`: {e:?}", K::kind(&()));
            }
        };

        Ok((
            Self {
                delete_on_fail,
                deleter: Some(deleter.boxed()),
            },
            created,
        ))
    }

    /// If the underlying resource should be deleted (e.g. current thread is not panicking or
    /// `delete_on_fail` was set), return the future to delete it. The future will
    /// delete the resource once awaited.
    ///
    /// Used to run deleters from multiple [`ResourceGuard`]s on one runtime.
    pub fn take_deleter(&mut self) -> Option<BoxFuture<'static, ()>> {
        if !std::thread::panicking() || self.delete_on_fail {
            self.deleter.take()
        } else {
            None
        }
    }
}

impl Drop for ResourceGuard {
    fn drop(&mut self) {
        let Some(deleter) = self.deleter.take() else {
            return;
        };

        if !std::thread::panicking() || self.delete_on_fail {
            let _ = std::thread::spawn(move || {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("failed to create tokio runtime")
                    .block_on(deleter);
            })
            .join();
        }
    }
}
