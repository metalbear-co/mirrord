//! This crate defines the mirrord-agent environment as it is prepared in its Pod spec.
//!
//! Beware that any changes made here must be backward compatible (except for changes to
//! [`mesh::MeshVendor`]).

pub mod checked_env;
pub mod envs;
pub mod mesh;
