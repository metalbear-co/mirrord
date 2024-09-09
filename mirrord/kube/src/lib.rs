#![feature(lazy_cell)]
#![feature(let_chains)]
#![feature(try_trait_v2)]
#![warn(clippy::indexing_slicing)]

//! # Features
//!
//! ## `incluster`
//!
//! Turn this feature on if you want this code to be run from inside a Kubernetes cluster.
//! It affects the way [`api::kubernetes::KubernetesAPI`] connects to created agents.
//! From outside of the cluster, [`kube`]s port forwarding is used.
//! From inside of the cluster, plain TCP connection is made.

// no other way to specify this dependency only if incluster is not set, so we explicitly use it
// to avoid lint error
use tokio_retry as _;

pub mod api;
pub mod error;
