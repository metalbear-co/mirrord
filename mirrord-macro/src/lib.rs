use proc_macro2::{Literal, Span};
use quote::quote;
use syn::{parse::Parser, punctuated::Punctuated, token::Comma, Block, DeriveInput, Ident, ItemFn};

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
            let name = format!("Fn{}", fn_name[0..1].to_uppercase() + &fn_name[1..]);
            Ident::new(&name, Span::call_site())
        });

        let static_name = ident_string.split("_detour").next().map(|fn_name| {
            let name = format!("FN_{}", fn_name.to_uppercase());
            Ident::new(&name, Span::call_site())
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

        // `pub(crate) type FnClose = unsafe extern "C" fn(i32) -> i32`
        let type_alias = quote! {
            #visibility type #type_name = #bare_fn
        };

        // `pub(crate) static FN_CLOSE: HookFn<FnClose> = HookFn::default()`
        let original_fn = quote! {
            #visibility static #static_name: crate::detour::HookFn<#type_name> =
                crate::detour::HookFn::default()
        };

        let output = quote! {
            #type_alias;

            #original_fn;

            #proper_function

        };

        output
    };

    // Here we return the equivalent of (1) and (2) for the ffi function, plus the annotated
    // function we received as `input`.
    proc_macro::TokenStream::from(output)
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
            let name = format!("Fn{}", fn_name[0..1].to_uppercase() + &fn_name[1..]);
            Ident::new(&name, Span::call_site())
        });

        let static_name = ident_string.split("_detour").next().map(|fn_name| {
            let name = format!("FN_{}", fn_name.to_uppercase());
            Ident::new(&name, Span::call_site())
        });

        let unsafety = signature.unsafety;
        let abi = signature.abi;

        let fn_args = signature
            .inputs
            .clone()
            .into_iter()
            .map(|fn_arg| match fn_arg {
                syn::FnArg::Receiver(_) => panic!("Hooks should not take any form of `self`!"),
                syn::FnArg::Typed(arg) => arg.ty,
            })
            .collect::<Vec<_>>();

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
            #visibility static #static_name: crate::detour::HookFn<#type_name> =
                crate::detour::HookFn::default()
        };

        let mut modified_function = proper_function.clone();
        modified_function.block.stmts = Block::parse_within
            .parse2(quote!(
                let __bypass = crate::detour::DetourGuard::new();
                if __bypass.is_none() {
                    return #static_name (#fn_arg_names);
                }
            ))
            .unwrap();
        modified_function
            .block
            .stmts
            .extend(proper_function.block.stmts.iter().cloned());

        let output = quote! {
            #type_alias;

            #original_fn;

            #modified_function

        };

        output
    };

    // Here we return the equivalent of (1) and (2) for the ffi function, plus the annotated
    // function we received as `input`.
    proc_macro::TokenStream::from(output)
}

enum FieldDefinitionMode {
    Option,
    UnwrapOption,
    Nested,
}

struct FieldDefinition {
    source_ident: Ident,
    target_ident: Ident,
    ty: syn::Type,
    env: Option<Literal>,
    default: Option<Literal>,
    mode: FieldDefinitionMode,
}

#[proc_macro_derive(
    MirrordConfig,
    attributes(mapto, skip_config, unwrap_option, from_env, default_value)
)]
pub fn mirrord_config(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    let DeriveInput {
        attrs,
        vis,
        ident,
        generics: _,
        data,
    } = syn::parse_macro_input!(input as DeriveInput);

    let mapped_struct_ident = attrs
        .iter()
        .find(|attr| attr.path.is_ident("mapto"))
        .and_then(|attr| attr.parse_args().ok())
        .unwrap_or_else(|| Ident::new(&format!("Mapped{}", ident), Span::call_site()));

    let mapped_fields = match data {
        syn::Data::Struct(data) => match data.fields {
            syn::Fields::Named(data) => data
                .named
                .into_iter()
                .filter(|field| {
                    !field
                        .attrs
                        .iter()
                        .any(|attr| attr.path.is_ident("skip_config"))
                })
                .collect(),
            _ => vec![],
        },
        _ => vec![],
    };

    let mapped_fields_definition = mapped_fields
        .iter()
        .filter_map(|field| {
            let ty = match &field.ty {
                syn::Type::Path(ty) => ty.path.segments.first().and_then(|seg| {
                    if seg.ident != "Option" {
                        Some((FieldDefinitionMode::Nested, field.ty.clone(), None, None))
                    } else {
                        let env = field
                            .attrs
                            .iter()
                            .find(|attr| attr.path.is_ident("from_env"))
                            .and_then(|field| field.parse_args::<Literal>().ok());

                        let default = field
                            .attrs
                            .iter()
                            .find(|attr| attr.path.is_ident("default_value"))
                            .and_then(|field| field.parse_args::<Literal>().ok());

                        if field.attrs.iter().any(|attr| {
                            attr.path.is_ident("unwrap_option")
                                || attr.path.is_ident("default_value")
                        }) {
                            match &seg.arguments {
                                syn::PathArguments::AngleBracketed(generics) => {
                                    generics.args.first().and_then(|arg| match arg {
                                        syn::GenericArgument::Type(ty) => Some((
                                            FieldDefinitionMode::UnwrapOption,
                                            ty.clone(),
                                            env,
                                            default,
                                        )),
                                        _ => None,
                                    })
                                }
                                _ => None,
                            }
                        } else {
                            Some((FieldDefinitionMode::Option, field.ty.clone(), env, default))
                        }
                    }
                }),
                _ => None,
            };

            field
                .attrs
                .iter()
                .find(|attr| attr.path.is_ident("mapto"))
                .and_then(|attr| attr.parse_args().ok())
                .or(field.ident.clone())
                .zip(field.ident.clone())
                .zip(ty)
        })
        .map(
            |((source_ident, target_ident), (mode, ty, env, default))| FieldDefinition {
                source_ident,
                target_ident,
                mode,
                ty,
                env,
                default,
            },
        )
        .collect::<Vec<_>>();

    let mapped_fields_definition_tokens = mapped_fields_definition.iter().map(
        |FieldDefinition {
             target_ident,
             ty,
             mode,
             ..
         }| {
            match mode {
                FieldDefinitionMode::Option | FieldDefinitionMode::UnwrapOption => {
                    quote! { #target_ident: #ty }
                }
                FieldDefinitionMode::Nested => {
                    quote! { #target_ident: <#ty as MirrordConfig>::Generated }
                }
            }
        },
    );

    let mapped_fields_impl_tokens = mapped_fields_definition.iter().map(
        |FieldDefinition {
             source_ident,
             target_ident,
             mode,
             default,
             env,
             ..
         }| {
            let default = match default {
                Some(default) => quote! { #default.parse().ok() },
                None => quote! { None }
            };

            let env = match env {
                Some(var) => quote! { std::env::var(#var).ok().and_then(|val| val.parse().ok()) },
                None => quote! { None }
            };

            match mode {
                FieldDefinitionMode::UnwrapOption => quote! {
                    #target_ident: #env.or(self.#source_ident).or(#default).ok_or(ConfigError::ValueNotProvided(stringify!(#source_ident).to_owned()))?
                },
                FieldDefinitionMode::Option => quote! {
                    #target_ident: #env.or(self.#source_ident).or(#default)
                },
                FieldDefinitionMode::Nested => quote! {
                    #target_ident: self.#source_ident.generate_config()?
                },
            }
        },
    );

    let mapped_struct = quote! {
        #[derive(Debug)]
        #vis struct #mapped_struct_ident {
            #(#mapped_fields_definition_tokens),*
        }
    };

    let output = quote! {
        #mapped_struct

        impl MirrordConfig for #ident {
            type Generated = #mapped_struct_ident;

            fn generate_config(self) -> Result<#mapped_struct_ident, ConfigError> {
                Ok(#mapped_struct_ident {
                    #(#mapped_fields_impl_tokens),*
                })
            }
        }
    };

    // println!("{}", output);

    proc_macro::TokenStream::from(output)
}
