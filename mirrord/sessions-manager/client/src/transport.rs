//! Where the sessions-manager control and data planes are reached.
//!
//! A [`SessionsManagerTransport`] opens the raw control-plane event stream and dials an
//! assignment's data plane. Everything above it — SSE decoding, liveness, reconnects, retries,
//! assignment deduplication — is shared by every transport.

mod direct;
mod operator;

use std::{future::Future, time::Duration};

pub use direct::DirectTransport;
use hyper::header::HeaderValue;
use mirrord_operator_websocket::connection::OperatorConnection;
use mirrord_protocol_io::ProtocolEndpoint;
use mirrord_sessions_manager_protocol::{
    AssignmentSubscription, ConnectionAssignment, ServiceScope,
};
pub use operator::OperatorTransport;
use secrecy::ExposeSecret;

use crate::{control_plane::ControlPlaneEventStream, error::SessionsManagerClientError};

/// One way of reaching a sessions-manager deployment, shared by the agent and intproxy clients.
///
/// Implementations must be cheap to clone: the agent clones its transport into every data-plane
/// upgrade task.
pub trait SessionsManagerTransport: Clone + Send + Sync + 'static {
    /// Opens the assignments SSE stream for `subscription` within `scope`.
    ///
    /// Called once per connection attempt, reconnects included.
    fn subscribe_assignments(
        &self,
        scope: &ServiceScope,
        subscription: &AssignmentSubscription,
    ) -> impl Future<Output = Result<ControlPlaneEventStream, SessionsManagerClientError>> + Send;

    /// Establishes the data-plane connection named by `assignment`.
    ///
    /// Connections are handed back undecorated, as the transport has no way to know whether its
    /// caller wants to drive the socket itself or hand it to a
    /// [`mirrord_protocol_io::Connection`] task. Wrapping is the caller's decision, taken once at
    /// the point of use.
    fn connect_data_plane<E: ProtocolEndpoint + Send + Unpin + 'static>(
        &self,
        assignment: ConnectionAssignment,
    ) -> impl Future<Output = Result<OperatorConnection<E>, SessionsManagerClientError>> + Send;

    /// Upper bound on a single [`Self::connect_data_plane`] attempt.
    fn connect_timeout(&self) -> Duration {
        Duration::from_secs(30)
    }
}

/// The assignment's one-use data-plane credential as a header value, kept out of logs.
fn assignment_authorization(
    assignment: &ConnectionAssignment,
) -> Result<HeaderValue, SessionsManagerClientError> {
    let mut authorization = HeaderValue::from_str(assignment.authorization.expose_secret())
        .map_err(|_| SessionsManagerClientError::InvalidAuthorization)?;
    authorization.set_sensitive(true);
    Ok(authorization)
}
