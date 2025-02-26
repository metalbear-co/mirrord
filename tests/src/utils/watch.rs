//! Utilities for watching test resources created in the Kubernetes cluster.

use std::{collections::HashMap, fmt};

use futures::StreamExt;
use k8s_openapi::api::core::v1::{Pod, Service};
use kube::{
    runtime::{
        self,
        watcher::{Config, Event},
        WatchStreamExt,
    },
    Api, Client, Resource,
};
use serde::de::DeserializeOwned;

type WatchCondition<R> = Box<dyn FnMut(&HashMap<String, R>) -> bool>;

/// Utility struct for waiting until test Kubernetes resources reach a desired state.
pub struct Watcher<R> {
    api: Api<R>,
    config: Config,
    resources: HashMap<String, R>,
    condition: WatchCondition<R>,
}

impl<R> Watcher<R>
where
    R: 'static + Resource<DynamicType = ()> + Clone + fmt::Debug + Send + DeserializeOwned,
{
    fn handle_event(&mut self, event: Event<R>) -> bool {
        match event {
            Event::Apply(resource) => {
                let name = resource.meta().name.clone().unwrap();
                self.resources.insert(name, resource);
                (self.condition)(&self.resources)
            }
            Event::Delete(resource) => {
                let name = resource.meta().name.as_deref().unwrap();
                self.resources.remove(name);
                (self.condition)(&self.resources)
            }
            Event::Init => {
                self.resources.clear();
                false
            }
            Event::InitApply(resource) => {
                let name = resource.meta().name.clone().unwrap();
                self.resources.insert(name, resource);
                false
            }
            Event::InitDone => (self.condition)(&self.resources),
        }
    }

    /// Creates a new instance of this watcher.
    ///
    /// Given [`Api`] and [`Config`] will be used to to create the [`runtime::watcher()`] stream.
    ///
    /// Given `condition` will be used to whether the set of observed resources has reached its
    /// desired state.
    pub fn new<C>(api: Api<R>, config: Config, condition: C) -> Self
    where
        C: 'static + FnMut(&HashMap<String, R>) -> bool,
    {
        Self {
            api,
            config,
            resources: Default::default(),
            condition: Box::new(condition),
        }
    }

    /// Runs this watches until the set of observed resources has reached its desired state.
    pub async fn run(&mut self) {
        let mut stream =
            Box::pin(runtime::watcher(self.api.clone(), self.config.clone()).default_backoff());

        loop {
            tokio::select! {
                item = stream.next() => match item {
                    Some(Ok(event)) => {
                        if self.handle_event(event) {
                            break;
                        }
                    },
                    Some(Err(error)) => {
                        println!("Watcher stream for {} {:?} encountered an error and will resume with backoff: {error}", R::kind(&()), self.config);
                    }
                    None => {
                        panic!("Watcher stream for {} {:?} unexpectedly finished", R::kind(&()), self.config);
                    }
                },
            }
        }
    }
}

/// Waits until the given [`Service`] has at least `min` ready [`Pod`].
///
/// Returns ready pods.
///
/// A [`Pod`] is considered to be ready when:
/// 1. It's in the `Running` phase
/// 2. All of its containers are ready
pub async fn wait_until_pods_ready(service: &Service, min: usize, client: Client) -> Vec<Pod> {
    let api = Api::<Pod>::namespaced(client, service.metadata.namespace.as_deref().unwrap());

    let selector = service
        .spec
        .as_ref()
        .unwrap()
        .selector
        .as_ref()
        .unwrap()
        .iter()
        .map(|entry| format!("{}={}", entry.0, entry.1))
        .collect::<Vec<_>>()
        .join(",");
    let config = Config {
        label_selector: Some(selector),
        ..Default::default()
    };

    fn is_ready(pod: &Pod) -> bool {
        let Some(status) = pod.status.as_ref() else {
            return false;
        };

        if status.phase.as_deref() != Some("Running") {
            return false;
        }

        let Some(containers) = status.container_statuses.as_ref() else {
            return false;
        };

        containers.iter().all(|status| status.ready)
    }

    let mut watcher = Watcher::new(api, config, move |map| {
        map.values().filter(|p| is_ready(p)).count() >= min
    });

    watcher.run().await;

    watcher.resources.into_values().filter(is_ready).collect()
}
