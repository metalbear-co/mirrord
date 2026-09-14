//! Client-side integration with the mirrord sessions-manager.
//!
//! The sessions-manager coordinates connections between mirrord agents and
//! intproxies. This crate implements the client side of that coordination for
//! both peers.
//!
//! [`AgentClient`] registers an agent replica with the sessions-manager and
//! starts an [`AgentControlPlane`], which receives connection assignments and
//! establishes the corresponding data-plane connections. [`IntproxyClient`]
//! registers an intproxy connection, waits for its assignment, and connects to
//! the assigned data plane.
//!
//! Communication with the sessions-manager is split into a control plane,
//! responsible for registration and assignment, and a data plane, which carries
//! the connection established for an assignment.

mod assignments;
mod client;
mod config;
mod control_plane;
mod credentials;
mod data_plane;
mod environment;
mod error;
mod retry;

pub use client::{AgentClient, AgentControlPlane, IntproxyClient, SessionsManagerConnectInfo};
pub use credentials::{CredentialProvider, SharedSecretCredentials};
pub use data_plane::{DataPlaneConnectRequest, DataPlaneTransport, WebSocketDataPlaneTransport};
pub use environment::{
    sessions_manager_environment, sessions_manager_replica_id, sessions_manager_service,
};
pub use error::{Result, SessionsManagerClientError};
