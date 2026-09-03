#![warn(clippy::indexing_slicing)]

mod argo_rollout;
mod ci_failure_artifact_preview;
mod cleanup;
#[cfg(feature = "cli")]
mod cli;
mod env;
mod file_ops;
mod http;
#[cfg(any(feature = "cli", feature = "operator"))]
mod ls;
mod targetless;
mod traffic;

mod dirty_iptables;
pub mod utils;
