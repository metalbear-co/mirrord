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

pub mod api;
pub mod error;
pub mod resolved;
