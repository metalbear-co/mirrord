//! # Procedural macros for mirrord
//!
//! - Use [`protocol_break()`] to mark code that should be revised when the crate major version is
//!   bumped.
//! - Use [`boxed_future()`] on an `async fn` whose future is large, so its callers' stack frames
//!   stay small.
#![deny(unused_crate_dependencies)]

extern crate proc_macro;

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use proc_macro2_diagnostics::SpanDiagnosticExt;
use quote::{format_ident, quote};
use semver::Version;
use syn::{FnArg, ItemFn, Pat, ReturnType, parse_macro_input, spanned::Spanned};

/// Marks code that should be revised when the major version of `mirrord-protocol` is bumped.
///
/// Will emit a compilation error on items that have been marked as needing revision when the
/// specified version is reached.
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

/// Makes an `async fn` build its future on the heap, inside a stack frame of its own.
///
/// A future is built on the stack of whoever awaits it before it moves into their state machine,
/// and a debug build gives every such temporary its own stack slot. A function that awaits a
/// different large future in each `match` arm needs room for all of them at once, every time it is
/// polled; the CLI's subcommand dispatch once took 748 KiB of the main thread's stack that way.
/// `Box::pin(f()).await` does not help: `f()` is still built in the caller's frame first.
///
/// With this attribute, `f()` itself puts the future on the heap, from a frame that is gone before
/// the future is first polled, and the caller only ever holds a pointer. Callers keep writing
/// `f().await`. Every call costs one allocation, which is nothing next to what these functions do.
///
/// ```ignore
/// #[mirrord_macros::boxed_future]
/// #[tracing::instrument(level = "trace", skip_all)]
/// async fn exec(args: &ExecArgs, watch: drain::Watch) -> CliResult<()> {
///     // ...
/// }
/// ```
///
/// Put it above `#[tracing::instrument]`, which then instruments the boxed future.
///
/// The function keeps an `async fn`'s contract: every argument lives in the future, unused ones
/// included, until it completes or is dropped, and argument patterns are matched inside it.
#[proc_macro_attribute]
pub fn boxed_future(attr: TokenStream, input: TokenStream) -> TokenStream {
    if !attr.is_empty() {
        return TokenStream2::from(attr)
            .span()
            .error("`boxed_future` takes no arguments")
            .emit_as_item_tokens()
            .into();
    }

    let mut function = parse_macro_input!(input as ItemFn);
    if function.sig.asyncness.take().is_none() {
        return function
            .sig
            .fn_token
            .span()
            .error("`boxed_future` goes on an `async fn`")
            .emit_as_item_tokens()
            .into();
    }
    // Inlining the function into its caller would put the future back in the caller's frame.
    if let Some(inline) = function
        .attrs
        .iter()
        .find(|attr| attr.path().is_ident("inline"))
    {
        return inline
            .span()
            .error("`boxed_future` needs the function out of line, so it can't take `#[inline]`")
            .emit_as_item_tokens()
            .into();
    }

    // Moves every argument into the future, as an `async fn` does. A plain identifier stays a
    // parameter and is only mentioned, which makes the `async move` block capture it; any other
    // pattern becomes a generated parameter that the pattern is matched against inside the future.
    let mut bindings = Vec::new();
    for (index, argument) in function.sig.inputs.iter_mut().enumerate() {
        match argument {
            FnArg::Receiver(_) => bindings.push(quote! { let _ = &self; }),
            FnArg::Typed(typed) => match &*typed.pat {
                Pat::Ident(ident) if ident.by_ref.is_none() && ident.subpat.is_none() => {
                    let name = &ident.ident;
                    bindings.push(quote! { let _ = &#name; });
                }
                _ => {
                    let generated = format_ident!("__arg{}", index);
                    let pattern = std::mem::replace(&mut *typed.pat, syn::parse_quote!(#generated));
                    bindings.push(match pattern {
                        Pat::Wild(_) => quote! { let _ = &#generated; },
                        pattern => quote! { let #pattern = #generated; },
                    });
                }
            },
        }
    }

    let output = match &function.sig.output {
        ReturnType::Default => quote!(()),
        ReturnType::Type(_, ty) => quote!(#ty),
    };
    function.sig.output = syn::parse_quote!(-> impl ::core::future::Future<Output = #output>);

    let body = &function.block;
    function.block = syn::parse_quote!({
        ::std::boxed::Box::pin(async move {
            #(#bindings)*
            #body
        })
    });
    function.attrs.push(syn::parse_quote!(#[inline(never)]));

    quote!(#function).into()
}
