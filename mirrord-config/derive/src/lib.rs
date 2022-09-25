use proc_macro2::{Span, TokenStream};
use proc_macro2_diagnostics::{Diagnostic, SpanDiagnosticExt};
use quote::quote;
use syn::{
    spanned::Spanned, Data, DataStruct, DeriveInput, Expr, Field, Fields, FieldsNamed,
    GenericArgument, Ident, Lit, Meta, NestedMeta, PathArguments, Type,
};

#[derive(Eq, PartialEq, Debug)]
enum FieldFlags {
    Unwrap,
    Nested,
    Env(Lit),
    Default(Lit),
}

/// Parse and create Ident from map_to attribute
fn map_to_ident(source: &Ident, expr: Option<Expr>) -> Ident {
    let fallback = Ident::new(&format!("Mapped{}", source), Span::call_site());

    if let Some(Expr::Assign(expr)) = expr {
        if let (Expr::Path(left_path), Expr::Path(right_path)) = (*expr.left, *expr.right) {
            if left_path.path.is_ident("map_to") {
                return right_path
                    .path
                    .get_ident()
                    .cloned()
                    .expect("map_to value not a valid Ident");
            }
        }
    }

    fallback
}

/// Parse field attribte to FieldFlags
fn get_config_flag(meta: &NestedMeta) -> Result<FieldFlags, Diagnostic> {
    match meta {
        NestedMeta::Meta(Meta::Path(path)) if path.is_ident("unwrap") => Ok(FieldFlags::Unwrap),
        NestedMeta::Meta(Meta::Path(path)) if path.is_ident("nested") => Ok(FieldFlags::Nested),
        NestedMeta::Meta(Meta::NameValue(meta)) if meta.path.is_ident("env") => {
            Ok(FieldFlags::Env(meta.lit.clone()))
        }
        NestedMeta::Meta(Meta::NameValue(meta)) if meta.path.is_ident("default") => {
            Ok(FieldFlags::Default(meta.lit.clone()))
        }
        _ => Err(meta.span().error("unsupported config attribute flag")),
    }
}

/// Parse field attribtes to FieldFlags
fn get_config_flags(meta: Meta) -> Result<Vec<FieldFlags>, Diagnostic> {
    let result = match meta {
        Meta::List(list) => {
            let mut result = Vec::new();
            for nested in list.nested.iter() {
                result.push(get_config_flag(nested)?);
            }

            result
        }
        _ => vec![],
    };

    Ok(result)
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
        .any(|flag| matches!(flag, FieldFlags::Unwrap | FieldFlags::Default(_)))
    {
        unwrap_option(&ty)?
    } else {
        &ty
    };

    let output = if flags.contains(&FieldFlags::Nested) {
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
        FieldFlags::Env(env) => Some(env),
        _ => acc,
    });

    if let Some(flag) = env_flag {
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
    );

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
        let mut named = Vec::new();

        for field in fields.clone() {
            named.push(map_field_name(field)?);
        }

        quote! { { #(#named),* } }
    };

    let mapped_fields_impl = {
        let mut named = Vec::new();

        for field in fields {
            named.push(map_field_name_impl(&ident, field)?);
        }

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
