//! Hand-rolled stand-ins for language/library features that are still nightly-only, so the
//! workspace can build on stable Rust.
//!
//! Each module here mirrors the path of the nightly feature it replaces (e.g.
//! [`std::error::Report`] -> [`error::Report`]), so a call site swaps its `use` and otherwise
//! reads the same as the nightly original.

pub mod error;
