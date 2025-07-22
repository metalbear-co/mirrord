//! This crate defines the mirrord-agent environment as it is prepared in its Pod spec.
//!
//! Beware that any changes made here must be backward compatible (except for changes to
//! [`mesh::MeshVendor`]).
//!
//! # Features
//!
//! * `k8s-openapi`: enables utility functions to produce `EnvVar` structs from
//!   [`CheckedEnv`](checked_env::CheckedEnv)s. Requires an extra dependency on `k8s-openapi` crate.
//! * `schema`: enables `JsonSchema` derives some structs that are used both in the agent and in the
//!   operator CRDs. Requires an extra dependency on `schemars` crate.
//!
//! # Default features
//!
//! This crate has no default features.

pub mod checked_env;
pub mod envs;
pub mod mesh;
pub mod steal_tls;
