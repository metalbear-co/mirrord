#![warn(clippy::indexing_slicing)]

mod argo_rollout;
mod cleanup;
mod cli;
mod env;
mod file_ops;
mod http;
mod ls;
mod operator_install;
mod targetless;
mod traffic;

mod dirty_iptables;
pub mod utils;
