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

pub static RETRY_KUBE_OPERATIONS: OnceLock<RetryConfig> = OnceLock::new();

#[derive(Debug, Clone)]
pub struct RetryConfig {
    pub exponential_backoff: ExponentialBackoff,
    pub max_attempts: usize,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            exponential_backoff: ExponentialBackoff::from_millis(100),
            max_attempts: 1,
        }
    }
}

impl RetryConfig {
    pub fn new(exponential_backoff: u64, max_attempts: usize) -> Self {
        Self {
            exponential_backoff: ExponentialBackoff::from_millis(exponential_backoff),
            max_attempts,
        }
    }
}

#[derive(Clone)]
pub struct BearApi<R> {
    kube_api: Api<R>,
    retry_config: RetryConfig,
}

impl<R: Resource> BearApi<R>
where
    <R as Resource>::DynamicType: Default,
{
    #[tracing::instrument(level = Level::INFO, skip(client))]
    pub fn all(client: Client) -> Self {
        BearApi {
            kube_api: Api::<R>::all(client),
            retry_config: RETRY_KUBE_OPERATIONS.get().cloned().unwrap_or_default(),
        }
    }

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
    #[tracing::instrument(level = Level::INFO, skip(self))]
    pub fn as_kube_api(&self) -> &Api<R> {
        &self.kube_api
    }

    #[tracing::instrument(level = Level::INFO, skip(self), ret, err)]
    pub async fn get(&self, name: &str) -> kube::Result<R> {
        Ok(self
            .retry_operation(|| async {
                tracing::info!("retrying ...");
                self.kube_api.get(name).await
            })
            .await?)
    }

    pub async fn get_subresource(&self, subresource_name: &str, name: &str) -> kube::Result<R> {
        Ok(self
            .retry_operation(|| async {
                tracing::info!("retrying ...");
                self.kube_api.get_subresource(subresource_name, name).await
            })
            .await?)
    }

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
    pub async fn log_stream(&self, name: &str, lp: &LogParams) -> kube::Result<impl AsyncBufRead> {
        Ok(self
            .retry_operation(|| async {
                tracing::info!("retrying ...");
                self.kube_api.log_stream(name, lp).await
            })
            .await?)
    }
}
