#![feature(try_trait_v2)]
#![warn(clippy::indexing_slicing)]
#![deny(unused_crate_dependencies)]
// TODO(alex): Get a big `Box` for the big variants.
#![allow(clippy::large_enum_variant)]

//! # Features
//!
//! ## `incluster`
//!
//! Turn this feature on if you want to connect to agent pods from within the cluster with a plain
//! TCP connection.
//!
//! ## `portforward`
//!
//! Turn this feature on if you want to connect to agent pods from outside the cluster with port
//! forwarding.

use std::sync::OnceLock;

use futures::AsyncBufRead;
use k8s_openapi::NamespaceResourceScope;
use kube::{
    Api, Client, Resource,
    api::{ListParams, Log, LogParams, ObjectList, PostParams},
};
use serde::{Serialize, de::DeserializeOwned};
use tokio_retry::{
    Action, RetryIf,
    strategy::{ExponentialBackoff, jitter},
};
use tracing::Level;

pub mod api;
pub mod error;
pub mod resolved;

/// Holds the [`RetryConfig`] that's to be used by the [`BearApi`] operations.
///
/// Is initialized after we have a `LayerConfig`.
pub static RETRY_KUBE_OPERATIONS: OnceLock<RetryConfig> = OnceLock::new();

/// Wraps the parameters for how we retry [`BearApi`] operations.
#[derive(Debug, Clone)]
pub struct RetryConfig {
    /// The retry strategy.
    pub exponential_backoff: ExponentialBackoff,

    /// Max amount of retries.
    ///
    /// Setting this to `0` means we run the operation once (no retries).
    pub max_attempts: usize,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            exponential_backoff: ExponentialBackoff::from_millis(0),
            max_attempts: 0,
        }
    }
}

impl RetryConfig {
    /// Initializes the [`RetryConfig`] from values that were set in a `LayerConfig`.
    ///
    /// The [`Default`] implementation sets these values to _zero-ish_, but when they come
    /// from the `LayerConfig`, the defaults there are actually `max_attempts: 1`, and
    /// `exponential_backoff: 50ms`, which means we retry stuff once.
    ///
    /// (alex): Currently this is all we're using it for, but if it becomes more general,
    /// update this comment.
    pub fn new(exponential_backoff: u64, max_attempts: usize) -> Self {
        Self {
            exponential_backoff: ExponentialBackoff::from_millis(exponential_backoff),
            max_attempts,
        }
    }
}

/// Wrapper over [`kube::Api`] that retries operations based on [`RetryConfig`].
#[derive(Clone)]
pub struct BearApi<R> {
    /// Inner [`kube::Api`] for the [`Resource`] `R`.
    kube_api: Api<R>,

    /// Defaults to retrying **once** when it's being loaded from a `LayerConfig`, through
    /// [`RETRY_KUBE_OPERATIONS`].
    retry_config: RetryConfig,
}

impl<R: Resource> BearApi<R>
where
    <R as Resource>::DynamicType: Default,
{
    /// [`kube::Api::all`] with retries.
    #[tracing::instrument(level = Level::INFO, skip(client))]
    pub fn all(client: Client) -> Self {
        BearApi {
            kube_api: Api::<R>::all(client),
            retry_config: RETRY_KUBE_OPERATIONS.get().cloned().unwrap_or_default(),
        }
    }

    /// [`kube::Api::namespaced`] with retries.
    #[tracing::instrument(level = Level::INFO, skip(client))]
    pub fn namespaced(client: Client, namespace: &str) -> Self
    where
        R: Resource<Scope = NamespaceResourceScope>,
    {
        BearApi {
            kube_api: Api::<R>::namespaced(client, namespace),
            retry_config: RETRY_KUBE_OPERATIONS.get().cloned().unwrap_or_default(),
        }
    }

    /// [`kube::Api::default_namespaced`] with retries.
    pub fn default_namespaced(client: Client) -> Self
    where
        R: Resource<Scope = NamespaceResourceScope>,
    {
        BearApi {
            kube_api: Api::<R>::default_namespaced(client),
            retry_config: RETRY_KUBE_OPERATIONS.get().cloned().unwrap_or_default(),
        }
    }
}

impl<R> BearApi<R>
where
    R: Clone + DeserializeOwned + std::fmt::Debug,
{
    /// Some operations don't require retrying, use this to get a reference to the inner
    /// [`kube::Api`].
    #[tracing::instrument(level = Level::INFO, skip(self))]
    pub fn as_kube_api(&self) -> &Api<R> {
        &self.kube_api
    }

    /// [`kube::Api::get`] with retries.
    #[tracing::instrument(level = Level::INFO, skip(self), ret, err)]
    pub async fn get(&self, name: &str) -> kube::Result<R> {
        Ok(self
            .retry_operation(|| async {
                tracing::info!("retrying ...");
                self.kube_api.get(name).await
            })
            .await?)
    }

    /// [`kube::Api::get_subresource`] with retries.
    pub async fn get_subresource(&self, subresource_name: &str, name: &str) -> kube::Result<R> {
        Ok(self
            .retry_operation(|| async {
                tracing::info!("retrying ...");
                self.kube_api.get_subresource(subresource_name, name).await
            })
            .await?)
    }

    /// [`kube::Api::create_subresource`] with retries.
    pub async fn create_subresource<T>(
        &self,
        subresource_name: &str,
        name: &str,
        pp: &PostParams,
        data: Vec<u8>,
    ) -> kube::Result<T>
    where
        T: DeserializeOwned,
    {
        Ok(self
            .retry_operation(|| async {
                tracing::info!("retrying ...");
                self.kube_api
                    .create_subresource::<T>(subresource_name, name, pp, data.clone())
                    .await
            })
            .await?)
    }

    /// [`kube::Api::replace_subresource`] with retries.
    pub async fn replace_subresource(
        &self,
        subresource_name: &str,
        name: &str,
        pp: &PostParams,
        data: Vec<u8>,
    ) -> kube::Result<R> {
        Ok(self
            .retry_operation(|| async {
                tracing::info!("retrying ...");
                self.kube_api
                    .replace_subresource(subresource_name, name, pp, data.clone())
                    .await
            })
            .await?)
    }

    /// [`kube::Api::create`] with retries.
    #[tracing::instrument(level = Level::INFO, skip(self), ret, err)]
    pub async fn create(&self, pp: &PostParams, data: &R) -> kube::Result<R>
    where
        R: Serialize,
    {
        Ok(self
            .retry_operation(|| async {
                tracing::info!("retrying ...");
                self.kube_api.create(pp, data).await
            })
            .await?)
    }

    /// [`kube::Api::list`] with retries.
    pub async fn list(&self, lp: &ListParams) -> kube::Result<ObjectList<R>> {
        Ok(self
            .retry_operation(|| async {
                tracing::info!("retrying ...");
                self.kube_api.list(lp).await
            })
            .await?)
    }
}

impl<R> BearApi<R>
where
    R: DeserializeOwned,
{
    /// Retries a [`kube::Api`] operation passed here as an [`Action`] (`async { ... }`) a number
    /// of times and with a exponential backoff strategy based on the [`RetryConfig`].
    ///
    /// Since we wrap the [`kube::Api`] functions in the [`Action`] that we call here, then
    /// setting a [`RetryConfig::max_attempts`] to `0` means "run this operation ONCE" (no retries).
    ///
    /// - Not all errors are retried, we only care about errors that have to do with cluster
    /// connectivity issues.
    ///
    /// - Why return `T` and not `R`? Some operations actually return a different type of
    /// [`Resource`] from their [`kube::Api<R>`].
    #[tracing::instrument(level = Level::INFO, skip(self, action), err)]
    async fn retry_operation<T, A>(&self, action: A) -> kube::Result<T>
    where
        A: Action<Item = T, Error = kube::Error>,
    {
        let retry_strategy = self
            .retry_config
            .exponential_backoff
            .clone()
            .map(jitter)
            .take(self.retry_config.max_attempts);

        let result = RetryIf::spawn(
            retry_strategy,
            action,
            (|fail| {
                matches!(
                    fail,
                    kube::Error::HyperError(..)
                        | kube::Error::Service(..)
                        | kube::Error::HttpError(..)
                        | kube::Error::Auth(..)
                        | kube::Error::UpgradeConnection(..)
                )
            }) as fn(&A::Error) -> bool,
        )
        .await;

        Ok(result?)
    }
}

impl<R> BearApi<R>
where
    R: DeserializeOwned + Log,
{
    /// [`kube::Api::log_stream`] with retries.
    pub async fn log_stream(&self, name: &str, lp: &LogParams) -> kube::Result<impl AsyncBufRead> {
        Ok(self
            .retry_operation(|| async {
                tracing::info!("retrying ...");
                self.kube_api.log_stream(name, lp).await
            })
            .await?)
    }
}
