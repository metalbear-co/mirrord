//! Common functionality for `layer-win`.

use std::fmt::Debug;

use mirrord_intproxy_protocol::{IsLayerRequest, IsLayerRequestWithResponse, MessageId};

use crate::{PROXY_CONNECTION, error::Error};

/// Helper function to make proxy request with response (Windows version).
pub fn make_proxy_request_with_response<T>(request: T) -> anyhow::Result<T::Response>
where
    T: IsLayerRequestWithResponse + std::fmt::Debug,
    T::Response: Debug,
{
    unsafe {
        Ok(PROXY_CONNECTION
            .get()
            .ok_or(Error::ProxyConnectionNotInitialized)?
            .make_request_with_response(request)
            .map_err(Error::ProxyConnectionError)?)
    }
}

/// Helper function to make proxy request with no response (Windows version).
pub fn make_proxy_request_no_response<T: IsLayerRequest + Debug>(
    request: T,
) -> anyhow::Result<MessageId> {
    unsafe {
        Ok(PROXY_CONNECTION
            .get()
            .ok_or(Error::ProxyConnectionNotInitialized)?
            .make_request_no_response(request)
            .map_err(Error::ProxyConnectionError)?)
    }
}
