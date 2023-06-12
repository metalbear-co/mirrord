extern crate proc_macro;

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use proc_macro2_diagnostics::SpanDiagnosticExt;
use semver::Version;
use syn::{parse_macro_input, spanned::Spanned};

/// Use [`protocol_break`] to mark code that should be revised when the major version is bumped.
/// For example:
/// #[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
/// pub enum StealType {
///     All(Port),
///     FilteredHttp(Port, Filter),
///     #[protocol_break(2)] // We want that when we bump major into 2, we can remove this variant
/// or merge it with the above.     FilteredHttpPath(Port, Filter),
/// }
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
