use proc_macro2::Span;
use quote::quote;
use syn::{
    Data, DataStruct, DeriveInput, Expr, Field, Fields, FieldsNamed, GenericArgument, Ident, Lit,
    Meta, NestedMeta, PathArguments, Type,
};

#[derive(Eq, PartialEq, Debug)]
enum FieldFlags {
    Unwrap,
    Nested,
    Env(Lit),
    Default(Lit),
}

fn map_to_ident(source: &Ident, expr: Option<Expr>) -> Ident {
    let fallback = Ident::new(&format!("Mapped{}", source), Span::call_site());

    if let Some(Expr::Assign(expr)) = expr {
        if let (Expr::Path(left_path), Expr::Path(right_path)) = (*expr.left, *expr.right) {
            if left_path.path.is_ident("map_to") {
                return right_path.path.get_ident().cloned().unwrap_or(fallback);
            }
        }
    }

    fallback
}

fn get_config_flag(meta: &NestedMeta) -> Option<FieldFlags> {
    if let NestedMeta::Meta(Meta::NameValue(meta)) = meta {
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

fn unwrap_option(ty: &Type) -> Option<&Type> {
    let seg = if let Type::Path(ty) = ty {
        ty.path.segments.first().expect("invalid segments")
    } else {
        panic!("invalid segments");
    };

    match &seg.arguments {
        PathArguments::AngleBracketed(generics) => {
            generics.args.first().and_then(|arg| match arg {
                GenericArgument::Type(ty) => Some(ty),
                _ => None,
            })
        }
        _ => None,
    }
}

fn get_config_flags(meta: Meta) -> Vec<FieldFlags> {
    match meta {
        Meta::List(list) => list
            .nested
            .iter()
            .filter_map(get_config_flag)
            .collect::<Vec<_>>(),
        _ => vec![],
    }
}

fn map_field_name(field: Field) -> proc_macro2::TokenStream {
    let Field {
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

fn map_field_name_impl(parent: &Ident, field: Field) -> proc_macro2::TokenStream {
    let Field { attrs, ident, .. } = field;

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
        |_| quote! { .ok_or(crate::config::ConfigError::ValueNotProvided(stringify!(#parent), stringify!(#ident)))? },
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
            .and_then(|attr| attr.parse_args::<Expr>().ok()),
    );

    let mapped_type = match data {
        Data::Struct(_) => quote! { struct },
        Data::Enum(_) => quote! { enum },
        _ => panic!("Unions are not supported"),
    };

    let mapped_fields = match &data {
        Data::Struct(DataStruct { fields, .. }) => match fields {
            Fields::Named(FieldsNamed { named, .. }) => {
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
        Data::Struct(DataStruct { fields, .. }) => match fields {
            Fields::Named(FieldsNamed { named, .. }) => {
                let named = named
                    .into_iter()
                    .map(|field| map_field_name_impl(&ident, field))
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
