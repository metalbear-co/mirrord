#![doc = include_str!("../README.md")]

use quote::ToTokens;
use syn::DeriveInput;

mod config;

#[doc = include_str!("../README.md")]
#[proc_macro_derive(MirrordConfig, attributes(config))]
pub fn mirrord_config(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    let input = syn::parse_macro_input!(input as DeriveInput);

    match config::ConfigStruct::new(input) {
        Ok(tokens) => tokens.into_token_stream().into(),
        Err(diag) => diag.emit_as_expr_tokens().into(),
    }
}
