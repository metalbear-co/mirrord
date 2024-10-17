#![deny(unused_crate_dependencies)]
#![feature(stmt_expr_attributes)]
#![warn(clippy::indexing_slicing)]

#[cfg(feature = "cli")]
mod cli;

mod env;
mod file_ops;
mod http;
mod issue1317;
mod operator;
mod targetless;
mod traffic;

pub mod utils;

#[cfg(test)]
mod deps_used_conditionally {
    //! To silence false positive from `unused_crate_dependencies`.
    //!
    //! These dependencies are used only when `operator` feature is enabled for this crate.
    //! However, dev dependencies cannot be optional.

    use aws_config as _;
    use aws_credential_types as _;
    use aws_sdk_sqs as _;
    use aws_types as _;
    use json_patch as _;
    use jsonptr as _;
    use mirrord_operator as _;
    use regex as _;
}
