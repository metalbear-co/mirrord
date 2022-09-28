use proc_macro2::{Span, TokenStream};
use proc_macro2_diagnostics::{Diagnostic, SpanDiagnosticExt};
use quote::quote;
use syn::{
    spanned::Spanned, Data, DataStruct, DeriveInput, Expr, Field, Fields, FieldsNamed,
    GenericArgument, Ident, Lit, Meta, NestedMeta, PathArguments, Type,
};

#[derive(Eq, PartialEq, Debug)]
enum FieldAttr {
    Unwrap,
    Nested,
    Env(Lit),
    Default(Lit),
}

/// Parse and create Ident from map_to attribute
fn map_to_ident(source: &Ident, expr: Option<Expr>) -> Result<Ident, Diagnostic> {
    let fallback = Ident::new(&format!("Mapped{}", source), Span::call_site());

    match expr {
        Some(Expr::Assign(expr)) => {
            let expr_span = expr.span();

            if let (Expr::Path(left_path), Expr::Path(right_path)) = (*expr.left, *expr.right) {
                if left_path.path.is_ident("map_to") {
                    return right_path
                        .path
                        .get_ident()
                        .cloned()
                        .ok_or_else(|| right_path.span().error("invalid ident"));
                }
            }

            Err(expr_span.error("invalid value for config attribute"))
        }
        Some(other) => Err(other.span().error("invalid value for config attribute")),
        None => Ok(fallback),
    }
}

/// Parse a flag in the `config` attribute on a struct field to FieldAttr
fn get_config_flag(meta: NestedMeta) -> Result<FieldAttr, Diagnostic> {
    match meta {
        NestedMeta::Meta(Meta::Path(path)) if path.is_ident("unwrap") => Ok(FieldAttr::Unwrap),
        NestedMeta::Meta(Meta::Path(path)) if path.is_ident("nested") => Ok(FieldAttr::Nested),
        NestedMeta::Meta(Meta::NameValue(meta)) if meta.path.is_ident("env") => {
            Ok(FieldAttr::Env(meta.lit))
        }
        NestedMeta::Meta(Meta::NameValue(meta)) if meta.path.is_ident("default") => {
            Ok(FieldAttr::Default(meta.lit))
        }
        _ => Err(meta.span().error("unsupported config attribute flag")),
    }
}

/// Parse the `config` attribute on a struct field
fn get_config_flags(meta: Meta) -> Result<Vec<FieldAttr>, Diagnostic> {
    match meta {
        Meta::List(list) => list.nested.into_iter().map(get_config_flag).collect(),
        _ => Ok(vec![]),
    }
}

/// Extract Type from Option<Type>
fn unwrap_option(ty: &Type) -> Result<&Type, Diagnostic> {
    let seg = if let Type::Path(ty) = ty {
        ty.path.segments.first().expect("invalid segments")
    } else {
        return Err(ty.span().error("invalid segments"));
    };

    match &seg.arguments {
        PathArguments::AngleBracketed(generics) => match generics.args.first() {
            Some(GenericArgument::Type(ty)) => Ok(ty),
            _ => Err(ty.span().error("called on not Option type")),
        },
        _ => Err(ty.span().error("called on not Option type")),
    }
}

/// Create struct definition
fn map_field_name(field: Field) -> Result<TokenStream, Diagnostic> {
    let Field {
        vis,
        attrs,
        ident,
        ty,
        ..
    } = field;

    let flags = {
        match attrs
            .iter()
            .find(|attr| attr.path.is_ident("config"))
            .and_then(|attr| attr.parse_meta().ok())
        {
            Some(meta) => get_config_flags(meta)?,
            None => Vec::new(),
        }
    };

    let ty = if flags
        .iter()
        .any(|flag| matches!(flag, FieldAttr::Unwrap | FieldAttr::Default(_)))
    {
        unwrap_option(&ty)?
    } else {
        &ty
    };

    let output = if flags.contains(&FieldAttr::Nested) {
        quote! {
            #vis #ident: <#ty as crate::config::MirrordConfig>::Generated
        }
    } else {
        quote! {
            #vis #ident: #ty
        }
    };

    Ok(output)
}

/// Create implementation for fields under generate_config
fn map_field_name_impl(parent: &Ident, field: Field) -> Result<TokenStream, Diagnostic> {
    let Field { attrs, ident, .. } = field;

    let flags = {
        match attrs
            .iter()
            .find(|attr| attr.path.is_ident("config"))
            .and_then(|attr| attr.parse_meta().ok())
        {
            Some(meta) => get_config_flags(meta)?,
            None => Vec::new(),
        }
    };

    let mut impls = Vec::new();

    let env_flag = flags.iter().fold(None, |acc, flag| match flag {
        FieldAttr::Env(env) => Some(env),
        _ => acc,
    });

    if let Some(flag) = env_flag {
        impls.push(quote! { crate::config::from_env::FromEnv::new(#flag) });
    }

    if flags.contains(&FieldAttr::Nested) {
        impls.push(quote! { Some(self.#ident.generate_config()?) })
    } else {
        impls.push(quote! { self.#ident.clone() });
    }

    if let Some(FieldAttr::Default(flag)) = flags
        .iter()
        .find(|flag| matches!(flag, FieldAttr::Default(_)))
    {
        impls.push(quote! { crate::config::default_value::DefaultValue::new(#flag) });
    }

    let unwrapper = flags.iter().find(|flag| matches!(flag, FieldAttr::Unwrap | FieldAttr::Nested | FieldAttr::Default(_))).map(
        |_| {
            let env_override = match env_flag {
                Some(flag) => quote! { Some(#flag) },
                None => quote! { None }
            };
            quote! { .ok_or(crate::config::ConfigError::ValueNotProvided(stringify!(#parent), stringify!(#ident), #env_override))? }
        }
    );

    let output = quote! {
        #ident: (#(#impls),*).source_value() #unwrapper
    };

    Ok(output)
}

/// Macro Logic
fn mirrord_config_macro(input: DeriveInput) -> Result<TokenStream, Diagnostic> {
    let DeriveInput {
        attrs,
        ident,
        data,
        generics,
        vis,
    } = input;

    let mapped_name = map_to_ident(
        &ident,
        attrs
            .iter()
            .find(|attr| attr.path.is_ident("config"))
            .and_then(|attr| attr.parse_args::<Expr>().ok()),
    )?;

    let mapped_type = match data {
        Data::Struct(_) => quote! { struct },
        _ => return Err(ident.span().error("Enums and Unions are not supported")),
    };

    let fields = match data {
        Data::Struct(DataStruct { fields, .. }) => match fields {
            Fields::Named(FieldsNamed { named, .. }) => named,
            _ => return Err(ident.span().error("Unnamed Structs are not supported")),
        },
        _ => return Err(ident.span().error("Enums and Unions are not supported")),
    };

    let mapped_fields = {
        let named = fields
            .clone()
            .into_iter()
            .map(map_field_name)
            .collect::<Result<Vec<_>, _>>()?;

        quote! { { #(#named),* } }
    };

    let mapped_fields_impl = {
        let named = fields
            .into_iter()
            .map(|field| map_field_name_impl(&ident, field))
            .collect::<Result<Vec<_>, _>>()?;

        quote! { { #(#named),* } }
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

    Ok(output)
}

#[proc_macro_derive(MirrordConfig, attributes(config))]
pub fn mirrord_config(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    let input = syn::parse_macro_input!(input as DeriveInput);

    match mirrord_config_macro(input) {
        Ok(tokens) => tokens.into(),
        Err(diag) => diag.emit_as_expr_tokens().into(),
    }
}
