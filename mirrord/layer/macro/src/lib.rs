#![warn(clippy::indexing_slicing)]

use proc_macro2::Span;
use quote::quote;
use syn::{Block, Ident, ItemFn, Type, parse::Parser, punctuated::Punctuated, token::Comma};

/// `#[hook_fn]` annotates the C ffi functions (mirrord's `_detour`s), and is used to generate the
/// following boilerplate (using `close_detour` as an example):
///
/// 1. `type FnClose = unsafe extern "C" fn(c_int) -> c_int`;
/// 2. `static FN_CLOSE: HookFn<FnClose> = HookFn(OnceLock::new())`;
///
/// Where (1) is the type alias of the ffi function, and (2) is where we'll store the original
/// function after calling replace with frida. `HookFn` is defined in `mirrord-layer` as a newtype
/// wrapper around `std::sync::Oncelock`.
///
///
/// The visibility of both (1) and (2) are based on the visibility of the annotated function.
///
/// `_args`: So far we're just ignoring this.
///
/// `input`: The ffi function, including docstrings, and other annotations.
///
/// -> Returns the `input` function with no loss of information (keeps docstrings and other
/// annotations intact).
#[proc_macro_attribute]
pub fn hook_fn(
    _args: proc_macro::TokenStream,
    input: proc_macro::TokenStream,
) -> proc_macro::TokenStream {
    let output: proc_macro2::TokenStream = {
        let proper_function = syn::parse_macro_input!(input as ItemFn);

        let signature = proper_function.clone().sig;
        let visibility = proper_function.clone().vis;

        let ident_string = signature.ident.to_string();
        let type_name = ident_string.split("_detour").next().map(|fn_name| {
            let (uppercase, lowercase) = fn_name.split_at(1);
            let name = format!("Fn{}{}", uppercase.to_uppercase(), lowercase);
            Ident::new(&name, Span::call_site())
        });

        let static_name = ident_string.split("_detour").next().map(|fn_name| {
            let name = format!("FN_{}", fn_name.to_uppercase());
            Ident::new(&name, Span::call_site())
        });

        let unsafety = signature.unsafety;
        let abi = signature.abi;

        // Function arguments without taking into account variadics!
        let mut fn_args = signature
            .inputs
            .into_iter()
            .map(|fn_arg| match fn_arg {
                syn::FnArg::Receiver(_) => panic!("Hooks should not take any form of `self`!"),
                syn::FnArg::Typed(arg) => arg.ty,
            })
            .collect::<Vec<_>>();

        // If we have `VaListImpl` args, then we push it to the end of the `fn_args` as
        // just `...`.
        if signature.variadic.is_some() {
            let fixed_arg = quote! {
                ...
            };

            fn_args.push(Box::new(Type::Verbatim(fixed_arg)));
        }

        let return_type = signature.output;

        // `unsafe extern "C" fn(i32) -> i32`
        let bare_fn = quote! {
            #unsafety #abi fn(#(#fn_args),*) #return_type
        };

        // `pub(crate) type FnClose = unsafe extern "C" fn(i32) -> i32`
        let type_alias = quote! {
            #visibility type #type_name = #bare_fn
        };

        // `pub(crate) static FN_CLOSE: HookFn<FnClose> = HookFn::default()`
        let original_fn = quote! {
            #visibility static #static_name: mirrord_layer_lib::detour::HookFn<#type_name> =
                mirrord_layer_lib::detour::HookFn::default_const()
        };

        let output = quote! {

            #[allow(non_camel_case_types)]
            #type_alias;

            #[allow(non_upper_case_globals)]
            #original_fn;

            #[allow(non_upper_case_globals)]
            #proper_function

        };

        output
    };

    // Here we return the equivalent of (1) and (2) for the ffi function, plus the annotated
    // function we received as `input`.
    proc_macro::TokenStream::from(output)
}

/// `#[internal_bypass(ORIGINAL)]` marks a layer-win detour so a call that belongs to mirrord
/// skips straight to the original function.
///
/// It answers that question twice, because a call belongs to mirrord in two ways.
///
/// 1. The thread is mirrord's own. The layer enables every hook before its startup worker connects
///    to the proxy; without this, that worker's own socket and file calls would be intercepted and
///    routed back through the not-yet-established connection. See
///    `utils-win/src/internal_thread.rs`, which `layer-win` re-exports as
///    `crate::hooks::internal_thread`.
/// 2. A detour is already running on the thread. A detour body reads the configuration, allocates,
///    logs, and talks to the proxy, and each of those can reach an API this layer hooks. Without
///    this the nested call comes back into the hook and the layer answers its own request with
///    remote state. See `layer-win/src/hooks/reentrancy.rs`, the port of the unix layer's
///    `DETOUR_BYPASS` and `#[hook_guard_fn]`.
///
/// The second mark lasts for one call. It is released while a detour body calls back into
/// application code; `reentrancy::ApplicationCallback` does that, and `socket::addrinfo_ex` is
/// the one place that needs it.
///
/// Do not put this on a hook that dispatches on what it was given rather than on who called it.
/// The list of those, and the reason for each, is in `utils-win/src/internal_thread.rs`.
///
/// `ORIGINAL` names the `OnceLock<&Fn>` static that `apply_hook!` fills at hook
/// creation (e.g. `SOCKET_ORIGINAL`). The annotated body is preserved verbatim and
/// only runs when neither mark is set.
///
/// Place this attribute as the outermost attribute on the function; it re-emits any
/// attributes below it (e.g. `#[instrument]`) so they still expand.
///
/// Only layer-win hooks have the `crate::hooks::internal_thread` and
/// `crate::hooks::reentrancy` modules; applying this in the unix layer will not compile.
#[proc_macro_attribute]
pub fn internal_bypass(
    args: proc_macro::TokenStream,
    input: proc_macro::TokenStream,
) -> proc_macro::TokenStream {
    let item = syn::parse_macro_input!(input as syn::ItemFn);
    let original = syn::parse_macro_input!(args as syn::Ident);

    let attrs = &item.attrs;
    let vis = &item.vis;
    let sig = &item.sig;
    let block = &item.block;

    let arg_names = sig
        .inputs
        .iter()
        .map(|input| match input {
            syn::FnArg::Receiver(_) => {
                panic!("internal_bypass cannot wrap a function taking `self`")
            }
            syn::FnArg::Typed(pat_type) => match pat_type.pat.as_ref() {
                syn::Pat::Ident(pat_ident) => pat_ident.ident.clone(),
                other => panic!(
                    "internal_bypass requires plain identifier parameters, found `{}`",
                    quote::quote!(#other)
                ),
            },
        })
        .collect::<Vec<_>>();

    let call_original = quote::quote! {
        let original = #original
            .get()
            .expect("internal_bypass: original function not set; hooks must be created before they can fire");
        return unsafe { original(#(#arg_names),*) };
    };

    let expanded = quote::quote! {
        #(#attrs)*
        #vis #sig {
            // Question one: is this thread mirrord's own?
            if crate::hooks::internal_thread::is_internal() {
                #call_original
            }

            // Question two: is a detour already running on this thread? The guard holds the mark
            // for the whole body, so every nested hooked call the body makes reaches the original.
            let __reentrancy = crate::hooks::reentrancy::BypassGuard::enter();
            if __reentrancy.is_none() {
                #call_original
            }

            #block
        }
    };

    proc_macro::TokenStream::from(expanded)
}

/// Same as above but calls the original function if detour guard is active.
#[proc_macro_attribute]
pub fn hook_guard_fn(
    _args: proc_macro::TokenStream,
    input: proc_macro::TokenStream,
) -> proc_macro::TokenStream {
    let output: proc_macro2::TokenStream = {
        let proper_function = syn::parse_macro_input!(input as ItemFn);

        let signature = proper_function.clone().sig;
        let visibility = proper_function.clone().vis;

        let ident_string = signature.ident.to_string();
        let type_name = ident_string.split("_detour").next().map(|fn_name| {
            let (uppercase, lowercase) = fn_name.split_at(1);
            let name = format!("Fn{}{}", uppercase.to_uppercase(), lowercase);
            Ident::new(&name, Span::call_site())
        });

        let static_name = ident_string.split("_detour").next().map(|fn_name| {
            let name = format!("FN_{}", fn_name.to_uppercase());
            Ident::new(&name, Span::call_site())
        });

        let unsafety = signature.unsafety;
        let abi = signature.abi;

        let mut fn_args = signature
            .inputs
            .clone()
            .into_iter()
            .map(|fn_arg| match fn_arg {
                syn::FnArg::Receiver(_) => panic!("Hooks should not take any form of `self`!"),
                syn::FnArg::Typed(arg) => arg.ty,
            })
            .collect::<Vec<_>>();

        // If we have `VaListImpl` args, then we push it to the end of the `fn_args` as
        // just `...`.
        if signature.variadic.is_some() {
            let fixed_arg = quote! {
                ...
            };

            fn_args.push(Box::new(Type::Verbatim(fixed_arg)));
        }

        let fn_arg_names: Punctuated<_, Comma> = signature
            .inputs
            .into_iter()
            .map(|fn_arg| match fn_arg {
                syn::FnArg::Receiver(_) => panic!("Hooks should not take any form of `self`!"),
                syn::FnArg::Typed(arg) => arg.pat,
            })
            .collect();

        let return_type = signature.output;

        // `unsafe extern "C" fn(i32) -> i32`
        let bare_fn = quote! {
            #unsafety #abi fn(#(#fn_args),*) #return_type
        };

        // `pub(crate) type FnClose = unsafe extern "C" fn(i32) -> i32`
        let type_alias = quote! {
            #visibility type #type_name = #bare_fn
        };

        // `pub(crate) static FN_CLOSE: HookFn<FnClose> = HookFn::default()`
        let original_fn = quote! {
            #visibility static #static_name: mirrord_layer_lib::detour::HookFn<#type_name> =
                mirrord_layer_lib::detour::HookFn::default_const()
        };

        let statements = proper_function.block.stmts.to_vec();
        let mut modified_function = proper_function;
        modified_function.block.stmts = Block::parse_within
            .parse2(quote!(
                let __bypass = mirrord_layer_lib::detour::DetourGuard::new();
                if __bypass.is_none() {
                    return #static_name (#fn_arg_names);
                }
            ))
            .unwrap();
        modified_function.block.stmts.extend(statements);

        let output = quote! {
            #[allow(non_camel_case_types)]
            #type_alias;

            #[allow(non_upper_case_globals)]
            #original_fn;

            #[allow(non_upper_case_globals)]
            #modified_function

        };

        output
    };

    // Here we return the equivalent of (1) and (2) for the ffi function, plus the annotated
    // function we received as `input`.
    proc_macro::TokenStream::from(output)
}

/// Wrapper for `tracing::instrument` that applies the tracing macro only if the debug assertions
/// are enabled. This is to reduce stack consumption in the layer and prevent segmentation faults.
///
/// Related issue: [Segmentation fault with microsocks](https://github.com/metalbear-co/mirrord/issues/2351).
///
/// **Warning**: `tracing` crate must be visible under name `tracing` at the call site of this
/// macro.
#[proc_macro_attribute]
pub fn instrument(
    args: proc_macro::TokenStream,
    item: proc_macro::TokenStream,
) -> proc_macro::TokenStream {
    let attr: proc_macro2::TokenStream = args.into();
    let item: proc_macro2::TokenStream = item.into();

    let output = quote! {
        #[cfg_attr(debug_assertions, tracing::instrument(#attr))]
        #item
    };

    proc_macro::TokenStream::from(output)
}

/// Expands to a call to the given `tracing` logging macro, but only when debug assertions are
/// enabled. In release builds the call is stripped entirely, leaving a `()` expression behind.
///
/// Like [`instrument`], this exists to keep `tracing` machinery out of release-build code paths in
/// the layer, where it adds stack consumption that can lead to segmentation faults.
///
/// `level` is the name of the `tracing` macro to delegate to (e.g. `trace`, `debug`).
fn conditional_log(level: &str, input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    let input: proc_macro2::TokenStream = input.into();
    let macro_name = Ident::new(level, Span::call_site());

    let output = quote! {
        {
            #[cfg(debug_assertions)]
            tracing::#macro_name!(#input);
        }
    };

    proc_macro::TokenStream::from(output)
}

/// Wrapper for `tracing::trace!` that is only emitted when debug assertions are enabled.
///
/// See [`conditional_log`] for the rationale.
///
/// **Warning**: the `tracing` crate must be visible under the name `tracing` at the call site.
#[proc_macro]
pub fn trace(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    conditional_log("trace", input)
}

/// Wrapper for `tracing::debug!` that is only emitted when debug assertions are enabled.
///
/// See [`conditional_log`] for the rationale.
///
/// **Warning**: the `tracing` crate must be visible under the name `tracing` at the call site.
#[proc_macro]
pub fn debug(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    conditional_log("debug", input)
}
