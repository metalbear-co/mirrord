use proc_macro2::Span;
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

fn map_to_ident(source: &Ident, expr: Option<syn::Expr>) -> Ident {
    let fallback = Ident::new(&format!("Mapped{}", source), Span::call_site());

    if let Some(syn::Expr::Assign(expr)) = expr {
        if let (syn::Expr::Path(left_path), syn::Expr::Path(right_path)) = (*expr.left, *expr.right)
        {
            if left_path.path.is_ident("map_to") {
                return right_path.path.get_ident().cloned().unwrap_or(fallback);
            }
        }
    }

    fallback
}

#[derive(Eq, PartialEq, Debug)]
enum FieldFlags {
    Unwrap,
    Nested,
    Env(syn::Lit),
    Default(syn::Lit),
}

fn get_config_flag(meta: &syn::NestedMeta) -> Option<FieldFlags> {
    if let syn::NestedMeta::Meta(syn::Meta::NameValue(meta)) = meta {
        if meta.path.is_ident("env") {
            return Some(FieldFlags::Env(meta.lit.clone()));
        }

        if meta.path.is_ident("default") {
            return Some(FieldFlags::Default(meta.lit.clone()));
        }

        if meta.path.is_ident("unwrap") {
            return Some(FieldFlags::Unwrap);
        }

        if meta.path.is_ident("nested") {
            return Some(FieldFlags::Nested);
        }
    }

    None
}

fn unwrap_option(ty: &syn::Type) -> Option<&syn::Type> {
    let seg = if let syn::Type::Path(ty) = ty {
        ty.path.segments.first().expect("invalid segments")
    } else {
        panic!("invalid segments");
    };

    match &seg.arguments {
        syn::PathArguments::AngleBracketed(generics) => {
            generics.args.first().and_then(|arg| match arg {
                syn::GenericArgument::Type(ty) => Some(ty),
                _ => None,
            })
        }
        _ => None,
    }
}

fn get_config_flags(meta: syn::Meta) -> Vec<FieldFlags> {
    match meta {
        syn::Meta::List(list) => list
            .nested
            .iter()
            .filter_map(get_config_flag)
            .collect::<Vec<_>>(),
        _ => vec![],
    }
}

fn map_field_name(field: syn::Field) -> proc_macro2::TokenStream {
    let syn::Field {
        vis,
        attrs,
        ident,
        ty,
        ..
    } = field;

    let flags = attrs
        .iter()
        .find(|attr| attr.path.is_ident("config"))
        .and_then(|attr| attr.parse_meta().ok())
        .map(get_config_flags)
        .unwrap_or_default();

    let ty = if flags
        .iter()
        .any(|flag| matches!(flag, FieldFlags::Unwrap | FieldFlags::Default(_)))
    {
        unwrap_option(&ty)
    } else {
        Some(&ty)
    };

    if flags.contains(&FieldFlags::Nested) {
        quote! {
            #vis #ident: <#ty as crate::config::MirrordConfig>::Generated
        }
    } else {
        quote! {
            #vis #ident: #ty
        }
    }
}

fn map_field_name_impl(field: syn::Field) -> proc_macro2::TokenStream {
    let syn::Field { attrs, ident, .. } = field;

    let flags = attrs
        .iter()
        .find(|attr| attr.path.is_ident("config"))
        .and_then(|attr| attr.parse_meta().ok())
        .map(get_config_flags)
        .unwrap_or_default();

    let mut impls = Vec::new();

    if let Some(FieldFlags::Env(flag)) =
        flags.iter().find(|flag| matches!(flag, FieldFlags::Env(_)))
    {
        impls.push(quote! { crate::config::from_env::FromEnv::new(#flag) });
    }

    if flags.contains(&FieldFlags::Nested) {
        impls.push(quote! { Some(self.#ident.generate_config()?) })
    } else {
        impls.push(quote! { self.#ident.clone() });
    }

    if let Some(FieldFlags::Default(flag)) = flags
        .iter()
        .find(|flag| matches!(flag, FieldFlags::Default(_)))
    {
        impls.push(quote! { crate::config::default_value::DefaultValue::new(#flag) });
    }

    let unwrapper = flags.iter().find(|flag| matches!(flag, FieldFlags::Unwrap | FieldFlags::Nested | FieldFlags::Default(_))).map(
        |_| quote! { .ok_or(crate::config::ConfigError::ValueNotProvided(stringify!(#ident).to_owned()))? },
    );

    quote! {
        #ident: (#(#impls),*).source_value() #unwrapper
    }
}

#[proc_macro_derive(MirrordConfig, attributes(config))]
pub fn mirrord_config(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    let DeriveInput {
        attrs,
        ident,
        data,
        generics,
        vis,
    } = syn::parse_macro_input!(input as DeriveInput);

    let mapped_name = map_to_ident(
        &ident,
        attrs
            .iter()
            .find(|attr| attr.path.is_ident("config"))
            .and_then(|attr| attr.parse_args::<syn::Expr>().ok()),
    );

    let mapped_type = match data {
        syn::Data::Struct(_) => quote! { struct },
        syn::Data::Enum(_) => quote! { enum },
        _ => panic!("Unions are not supported"),
    };

    let mapped_fields = match &data {
        syn::Data::Struct(syn::DataStruct { fields, .. }) => match fields {
            syn::Fields::Named(syn::FieldsNamed { named, .. }) => {
                let named = named
                    .clone()
                    .into_iter()
                    .map(map_field_name)
                    .collect::<Vec<_>>();

                quote! { { #(#named),* } }
            }
            _ => todo!(),
        },
        _ => todo!(),
    };

    let mapped_fields_impl = match data {
        syn::Data::Struct(syn::DataStruct { fields, .. }) => match fields {
            syn::Fields::Named(syn::FieldsNamed { named, .. }) => {
                let named = named
                    .into_iter()
                    .map(map_field_name_impl)
                    .collect::<Vec<_>>();

                quote! { { #(#named),* } }
            }
            _ => todo!(),
        },
        _ => todo!(),
    };

    let output = quote! {
        #[derive(Clone, Debug)]
        #vis #mapped_type #mapped_name #generics #mapped_fields

        impl crate::config::MirrordConfig for #ident {
            type Generated = #mapped_name;

            fn generate_config(self) -> Result<Self::Generated, crate::config::ConfigError> {
                Ok(#mapped_name #mapped_fields_impl)
            }
        }
    };

    proc_macro::TokenStream::from(output)
}
