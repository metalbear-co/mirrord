//! # proc macros for mirrord
//!
//! - Use [`protocol_break()`] to mark code that should be revised when the protocol major version
//!   (SemVer) is bumped.
#![deny(unused_crate_dependencies)]

extern crate proc_macro;

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use proc_macro2_diagnostics::SpanDiagnosticExt;
use semver::Version;
use syn::{parse_macro_input, spanned::Spanned};

/// Marks code that should be revised when the major version of `mirrord-protocol` is bumped.
///
/// Will emit an error on items that have been marked as needing revision when the specified version
/// is reached.
///
///  ### Example usage
///
/// ```
/// // In the `mirrord-protocol` crate
/// #[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
/// pub enum StealType {
///     All(Port),
///     FilteredHttp(Port, Filter),
///     // When we bump the major version to 2, we can remove this variant
///     // or merge it with the above.
///     #[protocol_break(2)]
///     FilteredHttpPath(Port, Filter),
/// }
/// ```
#[proc_macro_attribute]
pub fn protocol_break(attr: TokenStream, input: TokenStream) -> TokenStream {
    // Get the version to break on
    let break_on_str = parse_macro_input!(attr as syn::LitInt);
    let break_on_value = break_on_str.base10_parse::<u64>().unwrap();
    let input: TokenStream2 = input.into();
    if Version::parse(&std::env::var("CARGO_PKG_VERSION").unwrap())
        .unwrap()
        .major
        >= break_on_value
    {
        input
            .span()
            .error("This code should be revised when the major version is bumped.")
            .emit_as_expr_tokens()
            .into()
    } else {
        input.into()
    }
}
