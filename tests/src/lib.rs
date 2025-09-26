#![feature(stmt_expr_attributes)]
#![feature(ip)]
#![warn(clippy::indexing_slicing)]

mod argo_rollout;
mod cleanup;
#[cfg(feature = "cli")]
mod cli;
mod env;
mod file_ops;
mod http;
mod issue1317;
#[cfg(any(feature = "cli", feature = "operator"))]
mod ls;
#[cfg(all(feature = "operator", unix))]
mod operator;
mod targetless;
mod traffic;

mod dirty_iptables;
pub mod utils;
