use proc_macro2::Span;
use quote::{quote, ToTokens};
use syn::{AttributeArgs, Ident, ItemFn, MetaNameValue, Signature, TypeBareFn};

#[proc_macro_attribute]
pub fn hook_fn(
    args: proc_macro::TokenStream,
    input: proc_macro::TokenStream,
) -> proc_macro::TokenStream {
    let output: proc_macro2::TokenStream = {
        let macro_args = syn::parse_macro_input!(args as AttributeArgs);

        let mut hook_alias: Option<Ident> = None;

        for nested in macro_args.into_iter() {
            match nested {
                syn::NestedMeta::Meta(meta) => match meta {
                    syn::Meta::NameValue(name_value) => {
                        let ident = name_value.path.get_ident().expect(&format!(
                            "Expected ident for path, found {:#?}",
                            name_value.path
                        ));

                        if *ident == Ident::new("alias", Span::call_site()) {
                            hook_alias = Some(ident.clone());
                        } else {
                            panic!("Invalid ident {:#?} for macro!", ident);
                        }
                    }
                    invalid => panic!("Invalid meta arg {:#?} for macro!", invalid),
                },
                syn::NestedMeta::Lit(lit) => panic!("Invalid arg {:#?} for macro!", lit),
            }
        }

        let proper_function = syn::parse_macro_input!(input as ItemFn);

        let signature = proper_function.clone().sig;
        let detour_ident = signature.clone().ident;

        let visibility = proper_function.clone().vis;

        let ident_string = signature.ident.to_string();
        let type_name = ident_string.split("_").next().and_then(|fn_name| {
            let name = format!("Fn{}", fn_name[0..1].to_uppercase() + &fn_name[1..]);
            Some(Ident::new(&name, Span::call_site()))
        });

        let c_function_name = ident_string.split("_").next();
        let c_function_ident = ident_string
            .split("_")
            .next()
            .map(|n| Ident::new(n, Span::call_site()));

        let static_name = ident_string.split("_").next().and_then(|fn_name| {
            let name = format!("FN_{}", fn_name.to_uppercase());
            Some(Ident::new(&name, Span::call_site()))
        });

        let unsafety = signature.unsafety;
        let abi = signature.abi;

        let fn_args = signature
            .inputs
            .into_iter()
            .map(|fn_arg| match fn_arg {
                syn::FnArg::Receiver(_) => panic!("Hooks should not take any form of `self`!"),
                syn::FnArg::Typed(arg) => arg.ty,
            })
            .collect::<Vec<_>>();

        let return_type = signature.output;

        // `unsafe extern "C" fn(i32) -> i32`
        let bare_fn = quote! {
            #unsafety #abi fn(#(#fn_args),*) #return_type
        };

        // `pub type FnFoo = `
        let type_alias = quote! {
            #visibility type #type_name = #bare_fn
        };

        let original_fn = quote! {
            #visibility static #static_name: crate::HookFn<#type_name> =
                crate::HookFn(std::sync::OnceLock::new())
        };

        let output = quote! {
            #type_alias;

            #original_fn;

            #proper_function

        };

        // panic!("{}", output.to_string());

        output.into()
    };

    proc_macro::TokenStream::from(output)
}
