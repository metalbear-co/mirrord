//! Windows-only utilities shared across mirrord.
//!
//! This crate is the home for general Windows helpers. It is drawn on by the injected layer and
//! by the CLI.
//!
//! The Windows crash diagnostics also live here, under `diagnostics`. The crate is empty on
//! non-Windows targets.

#[cfg(windows)]
pub(crate) mod clipboard;
#[cfg(windows)]
pub mod diagnostics;
#[cfg(windows)]
pub mod error;
#[cfg(windows)]
pub mod fixed_buf;
#[cfg(windows)]
pub mod modules;
#[cfg(windows)]
pub mod process;
