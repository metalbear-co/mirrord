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

use http::{Request, Response};
use k8s_openapi::NamespaceResourceScope;
use kube::{
    Api, Client, Resource,
    api::{ListParams, ObjectList, PostParams},
    client::Body,
};
use serde::{Serialize, de::DeserializeOwned};
use tokio_retry::{
    Action, Retry, RetryIf,
    strategy::{ExponentialBackoff, jitter},
};
use tracing::Level;

pub mod api;
pub mod error;
pub mod resolved;

#[derive(Clone)]
pub struct BearClient {
    kube_client: Client,
    exponential_backoff: ExponentialBackoff,
    max_attempts: usize,
}

impl BearClient {
    #[tracing::instrument(level = Level::INFO, skip(self, action), err)]
    async fn retry_operation<T, A>(&self, action: A) -> kube::Result<T>
    where
        A: Action<Item = T, Error = kube::Error>,
    {
        let retry_strategy = self
            .exponential_backoff
            .clone()
            .map(jitter)
            .take(self.max_attempts);

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

    pub fn default_namespace(&self) -> &str {
        self.kube_client.default_namespace()
    }

    pub fn new(client: Client) -> Self {
        Self {
            kube_client: client,
            exponential_backoff: ExponentialBackoff::from_millis(2),
            max_attempts: 2,
        }
    }

    pub fn as_kube_client(&self) -> &Client {
        &self.kube_client
    }
}

#[derive(Clone)]
pub struct BearApi<R> {
    kube_api: Api<R>,
    exponential_backoff: ExponentialBackoff,
    max_attempts: usize,
}

impl<R: Resource> BearApi<R>
where
    <R as Resource>::DynamicType: Default,
{
    #[tracing::instrument(level = Level::INFO, skip(client))]
    pub fn all(client: BearClient) -> Self {
        BearApi {
            kube_api: Api::<R>::all(client.kube_client),
            exponential_backoff: ExponentialBackoff::from_millis(2),
            max_attempts: 2,
        }
    }

    #[tracing::instrument(level = Level::INFO, skip(client))]
    pub fn namespaced(client: BearClient, namespace: &str) -> Self
    where
        R: Resource<Scope = NamespaceResourceScope>,
    {
        BearApi {
            kube_api: Api::<R>::namespaced(client.kube_client, namespace),
            exponential_backoff: ExponentialBackoff::from_millis(2),
            max_attempts: 2,
        }
    }
}

impl<R> BearApi<R>
where
    R: Clone + DeserializeOwned + std::fmt::Debug,
{
    #[tracing::instrument(level = Level::INFO, skip(self, action), err)]
    async fn retry_operation<T, A>(&self, action: A) -> kube::Result<T>
    where
        A: Action<Item = T, Error = kube::Error>,
    {
        let retry_strategy = self
            .exponential_backoff
            .clone()
            .map(jitter)
            .take(self.max_attempts);

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
