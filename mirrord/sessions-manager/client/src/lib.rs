//! Client-side integration with the mirrord sessions-manager.
//!
//! The sessions-manager coordinates connections between mirrord agents and
//! intproxies. This crate implements the client side of that coordination for
//! both peers.
//!
//! [`AgentClient`] registers an agent replica with the sessions-manager and
//! runs its control plane in the background, receiving connection assignments
//! and establishing the corresponding data-plane connections. [`IntproxyClient`]
//! registers an intproxy connection, waits for its assignment, and connects to
//! the assigned data plane.
//!
//! Communication with the sessions-manager is split into a control plane,
//! responsible for registration and assignment, and a data plane, which carries
//! the connection established for an assignment. Both planes are reached through
//! a [`SessionsManagerTransport`]: [`DirectTransport`] for a standalone
//! sessions-manager, [`OperatorTransport`] for the one hosted by the mirrord
//! operator.

mod assignments;
mod client;
mod control_plane;
mod credentials;
mod error;
mod retry;
mod transport;

pub use client::{AgentClient, IntproxyClient, SessionsManagerConnectInfo};
pub use credentials::{CredentialProvider, SharedSecretCredentials};
pub use error::{Result, SessionsManagerClientError};
pub use mirrord_sessions_manager_protocol::{IntproxyIdentity, ReplicaId, ServiceScope};
pub use transport::{
    DirectTransport, OperatorTransport, SESSIONS_MANAGER_URL_ENV, SessionsManagerTransport,
};
