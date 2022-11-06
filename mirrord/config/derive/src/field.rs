use proc_macro2::TokenStream;
use quote::{quote, ToTokens};
use syn::{Field, GenericArgument, Ident, Lit, Meta, NestedMeta, PathArguments, Type, Visibility};

#[derive(Debug)]
pub struct FileStructField {
    pub flags: FileStructFieldFlags,
    pub ident: Option<Ident>,
    pub option: Option<Type>,
    pub ty: Type,
    pub vis: Visibility,
}

impl FileStructField {
    fn is_option(field: &Field) -> Option<Type> {
        let seg = if let Type::Path(ty) = &field.ty {
            ty.path.segments.first()?
        } else {
            return None;
        };

        match &seg.arguments {
            PathArguments::AngleBracketed(generics) => match generics.args.first() {
                Some(GenericArgument::Type(ty)) => Some(ty.clone()),
                _ => None,
            },
            _ => None,
        }
    }

    pub fn definition(&self) -> impl ToTokens {
        let FileStructField {
            ident,
            vis,
            ty,
            option,
            ..
        } = &self;

        let ty = option.as_ref().unwrap_or(ty);

        quote! { #vis #ident: Option<#ty> }
    }

    pub fn implmentation(&self, parent: &Ident) -> impl ToTokens {
        let FileStructField {
            ident,
            option,
            flags,
            ..
        } = &self;

        let mut impls = Vec::new();

        if let Some(env) = flags.env.as_ref() {
            impls.push(env.to_token_stream());
        }

        impls.push(quote! { self.#ident });

        if let Some(default) = flags.default.as_ref() {
            impls.push(default.to_token_stream());
        }

        let unwrapper = option.is_none().then(|| {
            let env_override = match &flags.env {
                Some(EnvFlag(flag)) => quote! { Some(#flag) },
                None => quote! { None }
            };

            quote! { .ok_or(crate::config::ConfigError::ValueNotProvided(stringify!(#parent), stringify!(#ident), #env_override))? }
        });

        quote! { #ident: (#(#impls),*).source_value()#unwrapper }
    }
}

impl From<Field> for FileStructField {
    fn from(field: Field) -> Self {
        let flags = FileStructFieldFlags::from(&field);
        let option = Self::is_option(&field);

        let Field { ident, vis, ty, .. } = field;

        FileStructField {
            flags,
            ident,
            option,
            ty,
            vis,
        }
    }
}

#[derive(Debug, Default)]
pub struct FileStructFieldFlags {
    pub env: Option<EnvFlag>,
    pub default: Option<DefaultFlag>,
}

impl From<&Field> for FileStructFieldFlags {
    fn from(field: &Field) -> Self {
        let mut flags = FileStructFieldFlags::default();

        for meta in field
            .attrs
            .iter()
            .filter(|attr| attr.path.is_ident("config"))
            .filter_map(|attr| attr.parse_meta().ok())
        {
            if let Meta::List(list) = meta {
                for meta in list.nested {
                    match meta {
                        NestedMeta::Meta(Meta::NameValue(meta)) if meta.path.is_ident("env") => {
                            flags.env = Some(EnvFlag(meta.lit))
                        }
                        NestedMeta::Meta(Meta::NameValue(meta))
                            if meta.path.is_ident("default") =>
                        {
                            flags.default = Some(DefaultFlag(meta.lit))
                        }
                        _ => {}
                    }
                }
            }
        }

        flags
    }
}

#[derive(Debug)]
pub struct EnvFlag(Lit);

impl ToTokens for EnvFlag {
    fn to_tokens(&self, tokens: &mut TokenStream) {
        let env_name = &self.0;

        tokens.extend(quote! { crate::config::from_env::FromEnv::new(#env_name) });
    }
}

#[derive(Debug)]
pub struct DefaultFlag(Lit);

impl ToTokens for DefaultFlag {
    fn to_tokens(&self, tokens: &mut TokenStream) {
        let default_value = &self.0;

        tokens.extend(quote! { crate::config::default_value::DefaultValue::new(#default_value) });
    }
}
