#![warn(clippy::indexing_slicing)]
#![deny(unused_crate_dependencies)]

//! WebSocket connection and upgrade support shared by operator API consumers.
//!
//! This crate contains the transport layer required to establish protocol connections through the
//! Kubernetes API server or a direct HTTPS endpoint. It deliberately excludes operator CRDs,
//! credentials, and higher-level operator API operations so the agent can use the transport
//! without depending on the full mirrord operator client stack.

use k8s_openapi as _;

pub mod connection;
pub mod upgrade;
