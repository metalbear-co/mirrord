//! This crate contains definition of the mirrord-agent environment prepared in its Pod spec.
//!
//! Beware that any changes made here must be backward compatible (except for changes to
//! [`mesh::MeshVendor`]).

pub mod checked_env;
pub mod envs;
pub mod mesh;
