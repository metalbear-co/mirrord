use proc_macro2::{Span, TokenStream};
use quote::{quote, ToTokens};
use syn::{Field, GenericArgument, Ident, Lit, Meta, NestedMeta, PathArguments, Type, Visibility};

pub struct FileStruct {
    pub vis: Visibility,
    pub ident: Ident,
    pub fields: Vec<FileStructField>,
    pub source: Ident,
}

impl FileStruct {
    pub fn new(vis: Visibility, source: Ident, fields: Vec<FileStructField>) -> Self {
        let ident = Ident::new(&format!("File{}", &source), Span::call_site());

        FileStruct {
            vis,
            source,
            ident,
            fields,
        }
    }
}

impl ToTokens for FileStruct {
    fn to_tokens(&self, tokens: &mut TokenStream) {
        let FileStruct {
            ident,
            vis,
            fields,
            source,
        } = &self;

        let field_definitions = fields.iter().map(|field| field.definition());
        let field_impl = fields.iter().map(|field| field.implmentation(&source));

        tokens.extend(quote! {
            #[derive(Debug, serde::Deserialize)]
            #vis struct #ident { #(#field_definitions),* }

            impl crate::config::MirrordConfig for #ident {
                type Generated = #source;

                fn generate_config(self) -> crate::config::Result<Self::Generated> {
                    Ok(#source {
                        #(#field_impl),*
                    })
                }
            }

            impl crate::config::FromMirrordConfig for #source {
                type Generator = #ident;
            }
        });
    }
}

pub struct FileStructField {
    pub vis: Visibility,
    pub ident: Option<Ident>,
    pub ty: Type,
    pub option: Option<Type>,
    pub env: Option<Lit>,
    pub default: Option<Lit>,
}

impl FileStructField {
    fn is_option(ty: &Type) -> Option<Type> {
        let seg = if let Type::Path(ty) = ty {
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

    fn field_meta(field: &Field) -> impl Iterator<Item = Meta> + '_ {
        field
            .attrs
            .iter()
            .filter(|attr| attr.path.is_ident("config"))
            .filter_map(|attr| attr.parse_meta().ok())
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
            env,
            default,
            ..
        } = &self;

        let mut impls = Vec::new();

        if let Some(env) = env.as_ref() {
            impls.push(quote! { crate::config::from_env::FromEnv::new(#env) });
        }

        impls.push(quote! { self.#ident });

        if let Some(default) = default.as_ref() {
            impls.push(quote! { crate::config::default_value::DefaultValue::new(#default) });
        }

        let unwrapper = option.is_none().then(|| {
            let env_override = match env {
                Some(flag) => quote! { Some(#flag) },
                None => quote! { None }
            };

            quote! { .ok_or(crate::config::ConfigError::ValueNotProvided(stringify!(#parent), stringify!(#ident), #env_override))? }
        });

        quote! { #ident: (#(#impls),*).source_value()#unwrapper }
    }
}

impl From<Field> for FileStructField {
    fn from(field: Field) -> Self {
        let mut env = None;
        let mut default = None;

        for meta in Self::field_meta(&field) {
            if let Meta::List(list) = meta {
                for meta in list.nested {
                    match meta {
                        NestedMeta::Meta(Meta::NameValue(meta)) if meta.path.is_ident("env") => {
                            env = Some(meta.lit)
                        }
                        NestedMeta::Meta(Meta::NameValue(meta))
                            if meta.path.is_ident("default") =>
                        {
                            default = Some(meta.lit)
                        }
                        _ => {}
                    }
                }
            }
        }

        let Field { ident, vis, ty, .. } = field;

        let option = Self::is_option(&ty);

        FileStructField {
            vis,
            ident,
            ty,
            option,
            env,
            default,
        }
    }
}
