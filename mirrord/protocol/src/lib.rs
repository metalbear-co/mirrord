//! This crate contains the definition of the mirrord-protocol,
//! which is used in communication between various components of mirrord, e.g. the CLI and the agent
//! or the CLI and the operator.
//!
//! # Protocol
//!
//! The protocol is based on [`bincode`]. It is defined **only** by the Rust types in this crate.
//!
//! Since mirrord components (namely: CLI, agent and operator) are distributed independently,
//! it is important to remember that the peer might not use the same version of the protocol.
//! This protocol must be kept backwards compatible.
//!
//! # Backwards compatibility
//!
//! To keep this protocol backwards compatible, limit your changes to:
//! 1. Renaming types, fields, or variants.
//! 2. Changing the type of a field, given that [`bincode`] representation is the same e.g. `T` ->
//!    `Arc<T>`.
//! 3. Adding a new variant to an enum, given that the new variant is the last one. Before sending
//!    the new variant, make sure that the peer can handle it (check their version of
//!    mirrord-protocol). We usually store version requirements in static variables, e.g.
//!    [`tcp::HTTP_FRAMED_VERSION`].
//! 4. Code unrelated to types' representation on the wire.
//!
//! Example changes that **do break** backwards compatibility:
//! 1. Adding a new field.
//! 2. Changing the type of a field from `T` to `Option<T>` and vice versa.
//! 3. Reordering variants of an enum.
//!
//! If you're not sure whether your change is breaking,
//! you can check it manually with a unit test.
//!
//! # Versioning
//!
//! The version of the protocol is stored in the [`VERSION`] static variable
//! and is the same as the version of this crate.
//!
//! Unlike the other crates in this workspace, this crate is versioned independently.
//!
//! Mind that CI checks that the version of this crate is bumped
//! whenever the files in this crate are modified, regardless of what the change actually is.
//!
//! When you make changes to this crate:
//! 1. If you're extending the protocol, bump minor version.
//! 2. Otherwise, bump patch version.
//!
//! # Utilities
//!
//! * [`ClientCodec`] and [`ProtocolCodec`] implement [`actix_codec::Decoder`] can be used with
//!   [`actix_codec::Framed`] transform an IO stream into
//!   [`Sink`](futures::Sink)+[`Stream`](futures::Stream) of mirrord-protocol messages.
//! * Some mirrord-protocol types implement conversion to/from [`socket2`] and [`hyper`] types.
//!   These should probably be implemented elsewhere (to remove dependencies from this crate), but
//!   right now we have these deps everywhere anyway, so it's not a big deal.

#![feature(const_trait_impl)]
#![feature(io_error_more)]
#![warn(clippy::indexing_slicing)]
#![deny(unused_crate_dependencies)]

pub mod batched_body;
pub mod codec;
pub mod dns;
pub mod error;
pub mod file;
pub mod outgoing;
#[deprecated = "pause feature was removed"]
pub mod pause;
pub mod tcp;
pub mod vpn;

use std::{collections::HashSet, ops::Deref, sync::LazyLock};

pub use codec::*;
pub use error::*;

pub type Port = u16;
pub type ConnectionId = u64;

/// An HTTP request ID, unique within a single incoming connection.
pub type RequestId = u16;

/// The version of this crate and the mirrord-protocol.
pub static VERSION: LazyLock<semver::Version> = LazyLock::new(|| {
    env!("CARGO_PKG_VERSION")
        .parse()
        .expect("Bad version parsing")
});

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct EnvVars(pub String);

impl From<EnvVars> for HashSet<String> {
    fn from(env_vars: EnvVars) -> Self {
        env_vars
            .split_terminator(';')
            .map(String::from)
            .collect::<HashSet<_>>()
    }
}

impl Deref for EnvVars {
    type Target = String;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
