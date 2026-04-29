use std::{io, net::IpAddr, sync::Arc};

use futures::{StreamExt, stream::FuturesOrdered};
use mirrord_protocol::{ResponseError, dns::ReverseDnsLookupResponse};
use tokio::{runtime::Handle, task::JoinHandle};

use crate::{
    error::{AgentError, AgentResult},
    task::BgTaskRuntime,
};

/// Handles [`ClientMessage::ReverseDnsLookup`](mirrord_protocol::codec::ClientMessage::ReverseDnsLookup) requests.
///
/// Every client connection should use its own instance.
///
/// # TODO
///
/// This is a temporary fix. We need to improve the [`mirrord_protocol`] around reverse lookups,
/// and use [`hickory_resolver`].
pub struct ReverseDnsApi {
    handle: Handle,
    /// [`FuturesOrdered`] guarantee that we produce responses in the correct order.
    results: FuturesOrdered<JoinHandle<io::Result<String>>>,
}

impl ReverseDnsApi {
    /// Creates a new instance, which will perform the lookups using tasks spawned on
    /// [`BgTaskRuntime::handle`].
    ///
    /// If this agent has a target, this runtime should live in the target's network namespace.
    pub fn new(network_runtime: &BgTaskRuntime) -> Self {
        Self {
            handle: network_runtime.handle().clone(),
            results: Default::default(),
        }
    }

    /// Issues an asynchronous reverse DNS lookup request.
    ///
    /// When available, the result will be returned from [`Self::recv`].
    pub fn request_reverse_lookup(&mut self, ip: IpAddr) {
        let task = self
            .handle
            .spawn_blocking(move || dns_lookup::lookup_addr(&ip));
        self.results.push_back(task);
    }

    /// Returns the result of the oldest request made with [`Self::request_reverse_lookup`].
    pub async fn recv(&mut self) -> AgentResult<ReverseDnsLookupResponse> {
        let Some(result) = self.results.next().await else {
            return std::future::pending().await;
        };
        let hostname = result
            .map_err(|error| AgentError::BackgroundTaskFailed {
                task: "reverse_lookup",
                error: Arc::new(error),
            })?
            .map_err(ResponseError::from);

        Ok(ReverseDnsLookupResponse { hostname })
    }
}
