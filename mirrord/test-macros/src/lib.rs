#![deny(unused_crate_dependencies)]

extern crate proc_macro;

use proc_macro::TokenStream;
use quote::quote;
use syn::{Expr, ItemFn, parse_macro_input};

/// Runs an async test body in a dedicated Tokio runtime, enforces a timeout,
/// aborts the test task if it times out, and shuts the runtime down in background.
///
/// Needed when the tested code starts a spawn_blocking task that hangs indefinitely, which prevents
/// the runtime from shutting down. When running with this macro, the tokio runtime shuts down
/// without waiting for the hanging task.
///
/// Usage:
/// - `#[background_shutdown_tokio_test]` (no timeout)
/// - `#[timeout(std::time::Duration::from_secs(30))]` for optional timeout
#[proc_macro_attribute]
pub fn background_shutdown_tokio_test(attr: TokenStream, item: TokenStream) -> TokenStream {
    if !attr.is_empty() {
        return syn::Error::new(
            proc_macro2::Span::call_site(),
            "background_shutdown_tokio_test does not accept arguments; use #[timeout(...)]",
        )
        .to_compile_error()
        .into();
    }

    let input = parse_macro_input!(item as ItemFn);
    let vis = &input.vis;
    let sig = &input.sig;
    let name = &sig.ident;
    let block = &input.block;
    let mut passthrough_attrs = Vec::new();
    let mut timeout_expr = None::<Expr>;

    if sig.asyncness.is_none() {
        return syn::Error::new_spanned(sig.fn_token, "test function must be async")
            .to_compile_error()
            .into();
    }
    if !sig.inputs.is_empty() {
        return syn::Error::new_spanned(&sig.inputs, "test function must not take arguments")
            .to_compile_error()
            .into();
    }
    if sig.generics.lt_token.is_some() {
        return syn::Error::new_spanned(&sig.generics, "test function must not be generic")
            .to_compile_error()
            .into();
    }

    for attr in &input.attrs {
        if attr.path().is_ident("timeout") {
            if timeout_expr.is_some() {
                return syn::Error::new_spanned(attr, "duplicate #[timeout(...)] attribute")
                    .to_compile_error()
                    .into();
            }

            match attr.parse_args::<Expr>() {
                Ok(expr) => timeout_expr = Some(expr),
                Err(_) => {
                    return syn::Error::new_spanned(
                        attr,
                        "expected #[timeout(<Duration expression>)]",
                    )
                    .to_compile_error()
                    .into();
                }
            }
        } else {
            passthrough_attrs.push(attr);
        }
    }

    let run_body = if let Some(timeout) = timeout_expr {
        quote! {
            let test_timeout = #timeout;
            let mut task = ::tokio::spawn(async #block);
            match ::tokio::time::timeout(test_timeout, &mut task).await {
                Ok(Ok(())) => {}
                Ok(Err(err)) => panic!("test task failed: {err}"),
                Err(_) => {
                    task.abort();
                    let _ = task.await;
                    panic!("test timed out after {:?} and was aborted", test_timeout);
                }
            }
        }
    } else {
        quote! {
            let task = ::tokio::spawn(async #block);
            match task.await {
                Ok(()) => {}
                Err(err) => panic!("test task failed: {err}"),
            }
        }
    };

    quote! {
        #(#passthrough_attrs)*
        #[test]
        #vis fn #name() {
            let runtime = ::tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build tokio runtime for test");

            runtime.block_on(async {
                #run_body
            });

            runtime.shutdown_background();
        }
    }
    .into()
}
