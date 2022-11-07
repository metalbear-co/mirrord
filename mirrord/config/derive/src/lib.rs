use quote::ToTokens;
use syn::DeriveInput;

mod field;
mod file;
mod flag;

#[proc_macro_derive(MirrordConfig, attributes(config))]
pub fn mirrord_config2(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    let input = syn::parse_macro_input!(input as DeriveInput);

    match file::FileStruct::new(input).map(|file| file.into_token_stream()) {
        Ok(tokens) => tokens.into(),
        Err(diag) => diag.emit_as_expr_tokens().into(),
    }
}
