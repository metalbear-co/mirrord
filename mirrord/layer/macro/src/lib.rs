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
///    remote state. This is the `mirrord_layer_lib::detour::DetourGuard` that `#[hook_guard_fn]`
///    takes in the unix layer.
///
/// The second mark lasts for one call. `mirrord_layer_lib::detour::ApplicationCallback` releases
/// it around a call from a detour body into application code.
///
/// # Hooks that dispatch on a value
///
/// Some hooks decide by what they were given rather than by who called: a handle or socket the
/// layer handed out has to be served by the layer whoever passes it back, or the kernel receives
/// a value it never issued. `#[internal_bypass(ORIGINAL, managed = EXPR)]` covers them. `EXPR` is
/// evaluated, with the parameters in scope, only when one of the two checks would bypass, so the
/// common call pays for no lookup. When it is `true` the body runs even on an internal thread or
/// inside another detour. The body still holds the mark, its own guard when it is the outermost
/// detour and the enclosing detour's otherwise, so its nested calls reach the originals.
///
/// A hook whose every value can be the layer's, such as the `freeaddrinfo` family, takes no
/// bypass at all; its own comment says why.
///
/// # Arguments
///
/// `ORIGINAL` names the `OnceLock` static that holds the original function once the hook is
/// created (e.g. `SOCKET_ORIGINAL`). The annotated body is preserved verbatim.
///
/// # Attributes
///
/// Place this attribute as the outermost attribute on the function. An `instrument` attribute
/// below it moves, with the body, into an inner function that runs only after both checks, so a
/// bypassed call opens no span and the `ret` event is written while the mark is still held. The
/// span keeps the hook's name. Lint attributes apply to both functions, and every other attribute
/// stays on the hook.
///
/// Only layer-win hooks have the `crate::hooks::internal_thread` module; applying this in the unix
/// layer will not compile.
#[proc_macro_attribute]
pub fn internal_bypass(
    args: proc_macro::TokenStream,
    input: proc_macro::TokenStream,
) -> proc_macro::TokenStream {
    let item = syn::parse_macro_input!(input as syn::ItemFn);
    let InternalBypassArgs { original, managed } =
        syn::parse_macro_input!(args as InternalBypassArgs);

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

    let dispatch = match managed {
        None => quote::quote! {
            // Question one: is this thread mirrord's own?
            if crate::hooks::internal_thread::is_internal() {
                #call_original
            }

            // Question two: is a detour already running on this thread? The guard holds the mark
            // for the whole body, so every nested hooked call the body makes reaches the original.
            let __reentrancy = mirrord_layer_lib::detour::DetourGuard::new();
            if __reentrancy.is_none() {
                #call_original
            }
        },
        // The same two questions, and `managed` only when one of them would bypass: a value this
        // layer handed out is served by the layer whoever passes it back. A nested call gets no
        // guard of its own, and the enclosing detour's mark still covers the body's work.
        Some(managed) => quote::quote! {
            let __reentrancy = if !crate::hooks::internal_thread::is_internal() {
                let __reentrancy = mirrord_layer_lib::detour::DetourGuard::new();
                if __reentrancy.is_none() && !(#managed) {
                    #call_original
                }
                __reentrancy
            } else if #managed {
                mirrord_layer_lib::detour::DetourGuard::new()
            } else {
                #call_original
            };
        },
    };

    let (traced_attrs, lint_attrs, hook_attrs) = partition_bypass_attrs(&item.attrs, &sig.ident);

    let body = if traced_attrs.is_empty() {
        quote::quote! { #block }
    } else {
        traced_body(sig, block, &arg_names, &traced_attrs, &lint_attrs)
    };

    // The hook's own parameters are only forwarded when the body moved into an inner function,
    // so a `mut` on them would be unused there.
    let mut hook_sig = sig.clone();
    if !traced_attrs.is_empty() {
        strip_binding_mode(&mut hook_sig);
    }

    let expanded = quote::quote! {
        #(#hook_attrs)*
        #(#lint_attrs)*
        #vis #hook_sig {
            #dispatch

            #body
        }
    };

    proc_macro::TokenStream::from(expanded)
}

/// The arguments of [`internal_bypass`]: `ORIGINAL` and an optional `managed = EXPR`.
struct InternalBypassArgs {
    original: Ident,
    managed: Option<syn::Expr>,
}

impl syn::parse::Parse for InternalBypassArgs {
    fn parse(input: syn::parse::ParseStream) -> syn::Result<Self> {
        let original = input.parse()?;
        let mut managed = None;

        if input.parse::<Option<syn::Token![,]>>()?.is_some() && !input.is_empty() {
            let key: Ident = input.parse()?;
            if key != "managed" {
                return Err(syn::Error::new(key.span(), "expected `managed = <expr>`"));
            }
            input.parse::<syn::Token![=]>()?;
            managed = Some(input.parse()?);
            input.parse::<Option<syn::Token![,]>>()?;
        }

        Ok(Self { original, managed })
    }
}

/// Splits a hook's attributes into the `instrument` ones (renamed to the hook), the lint ones,
/// and the rest.
fn partition_bypass_attrs(
    attrs: &[syn::Attribute],
    hook: &Ident,
) -> (
    Vec<syn::Attribute>,
    Vec<syn::Attribute>,
    Vec<syn::Attribute>,
) {
    let last_segment_is = |attr: &syn::Attribute, names: &[&str]| {
        attr.path()
            .segments
            .last()
            .is_some_and(|segment| names.iter().any(|name| segment.ident == name))
    };

    let mut traced = Vec::new();
    let mut lints = Vec::new();
    let mut rest = Vec::new();
    for attr in attrs {
        if last_segment_is(attr, &["instrument"]) {
            traced.push(named_after(attr, hook));
        } else if attr.path().get_ident().is_some()
            && last_segment_is(attr, &["allow", "expect", "warn", "deny"])
        {
            lints.push(attr.clone());
        } else {
            rest.push(attr.clone());
        }
    }

    (traced, lints, rest)
}

/// Gives an `instrument` attribute `name = "<hook>"` unless it names its span already, because
/// the function it lands on is the inner one.
fn named_after(attr: &syn::Attribute, hook: &Ident) -> syn::Attribute {
    let name = hook.to_string();
    let path = attr.path();

    match &attr.meta {
        syn::Meta::Path(_) => syn::parse_quote!(#[#path(name = #name)]),
        syn::Meta::List(list) => {
            let mut tokens = list.tokens.clone().into_iter().peekable();
            let mut named = false;
            while let Some(token) = tokens.next() {
                if let proc_macro2::TokenTree::Ident(ident) = &token
                    && ident == "name"
                    && matches!(
                        tokens.peek(),
                        Some(proc_macro2::TokenTree::Punct(punct)) if punct.as_char() == '='
                    )
                {
                    named = true;
                    break;
                }
            }

            if named {
                attr.clone()
            } else if list.tokens.is_empty() {
                syn::parse_quote!(#[#path(name = #name)])
            } else {
                let inner = &list.tokens;
                syn::parse_quote!(#[#path(name = #name, #inner)])
            }
        }
        syn::Meta::NameValue(_) => attr.clone(),
    }
}

/// Removes `mut` and `ref` from every parameter binding, which leaves the function's type alone.
fn strip_binding_mode(sig: &mut syn::Signature) {
    for input in sig.inputs.iter_mut() {
        if let syn::FnArg::Typed(pat_type) = input
            && let syn::Pat::Ident(pat_ident) = pat_type.pat.as_mut()
        {
            pat_ident.mutability = None;
            pat_ident.by_ref = None;
        }
    }
}

/// The body of a hook that carries `instrument`, as one inner function, `__mirrord_traced`, that
/// carries the tracing attributes and holds the annotated block verbatim.
///
/// The `ret` event and the span exit run after the body has set the last error its caller reads.
/// The layer's subscriber keeps that error across every callback, so nothing here restores it.
fn traced_body(
    sig: &syn::Signature,
    block: &Block,
    arg_names: &[Ident],
    traced_attrs: &[syn::Attribute],
    lint_attrs: &[syn::Attribute],
) -> proc_macro2::TokenStream {
    let unsafety = &sig.unsafety;
    let generics = &sig.generics;
    let where_clause = &sig.generics.where_clause;
    let inputs = &sig.inputs;
    let output = &sig.output;

    let call = quote::quote! { __mirrord_traced(#(#arg_names),*) };
    let call = match unsafety {
        Some(_) => quote::quote! { unsafe { #call } },
        None => call,
    };

    quote::quote! {
        #(#lint_attrs)*
        #(#traced_attrs)*
        #[inline(always)]
        #unsafety fn __mirrord_traced #generics (#inputs) #output #where_clause #block

        #call
    }
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
