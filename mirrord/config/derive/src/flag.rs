use proc_macro2::{Span, TokenStream};
use quote::{quote, ToTokens};
use syn::{Attribute, Ident, Lit, Meta, NestedMeta};

#[derive(Debug, Default)]
pub struct ConfigFlags {
    pub map_to: Option<Ident>,
    pub env: Option<EnvFlag>,
    pub default: Option<DefaultFlag>,
}

impl From<&Vec<Attribute>> for ConfigFlags {
    fn from(attrs: &Vec<Attribute>) -> Self {
        let mut flags = ConfigFlags::default();

        for meta in attrs
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
                        NestedMeta::Meta(Meta::NameValue(meta)) if meta.path.is_ident("map_to") => {
                            match meta.lit {
                                Lit::Str(val) => {
                                    flags.map_to = Some(Ident::new(&val.value(), Span::call_site()))
                                }
                                _ => {}
                            }
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
pub struct EnvFlag(pub Lit);

impl ToTokens for EnvFlag {
    fn to_tokens(&self, tokens: &mut TokenStream) {
        let env_name = &self.0;

        tokens.extend(quote! { crate::config::from_env::FromEnv::new(#env_name) });
    }
}

#[derive(Debug)]
pub struct DefaultFlag(pub Lit);

impl ToTokens for DefaultFlag {
    fn to_tokens(&self, tokens: &mut TokenStream) {
        let default_value = &self.0;

        tokens.extend(quote! { crate::config::default_value::DefaultValue::new(#default_value) });
    }
}
