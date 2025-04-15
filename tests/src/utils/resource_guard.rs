use std::fmt::Debug;

use futures::FutureExt;
use futures_util::future::BoxFuture;
use kube::{
    api::{DeleteParams, PostParams},
    Api, Error, Resource,
};
use serde::{de::DeserializeOwned, Serialize};

/// RAII-style guard for deleting kube resources after tests.
/// This guard deletes the kube resource when dropped.
/// This guard can be configured not to delete the resource if dropped during a panic.
pub(crate) struct ResourceGuard {
    /// Whether the resource should be deleted if the test has panicked.
    pub(crate) delete_on_fail: bool,
    /// This future will delete the resource once awaited.
    pub(crate) deleter: Option<BoxFuture<'static, ()>>,
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
        println!("Creating {} `{name}`: {data:?}", K::kind(&()));
        let created = api.create(&PostParams::default(), data).await?;
        println!("Created {} `{name}`", K::kind(&()));

        let deleter = async move {
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
