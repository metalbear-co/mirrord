use proc_macro2::{Span, TokenStream};
use proc_macro2_diagnostics::{Diagnostic, SpanDiagnosticExt};
use quote::{quote, ToTokens};
use syn::{spanned::Spanned, Attribute, Ident, Lit, Meta, NestedMeta};

#[derive(Debug, Eq, PartialEq)]
pub enum ConfigFlagsType {
    Container,
    Field,
}

/// Contains flags parsed from `#[config(...)]` and `#[doc]` attributes
///
/// ConfigFlagsType::Container -> ["derive", "generator", "map_to"]
/// ConfigFlagsType::Field -> ["default", "env", "nested", "rename", "toggleable"]
#[derive(Debug, Default)]
pub struct ConfigFlags {
    pub doc: Vec<Attribute>,

    pub derive: Vec<Ident>,
    pub generator: Option<Ident>,
    pub map_to: Option<Ident>,

    pub default: Option<DefaultFlag>,
    pub env: Option<EnvFlag>,
    pub nested: bool,
    pub rename: Option<Lit>,
    pub toggleable: bool,
    pub unstable: bool,
    pub deprecated: Option<Lit>,
}

impl ConfigFlags {
    pub fn new(attrs: &[Attribute], mode: ConfigFlagsType) -> Result<Self, Diagnostic> {
        let mut flags = ConfigFlags {
            doc: attrs
                .iter()
                .filter(|attr| attr.path.is_ident("doc"))
                .cloned()
                .collect(),
            ..Default::default()
        };

        for meta in attrs
            .iter()
            .filter(|attr| attr.path.is_ident("config"))
            .filter_map(|attr| attr.parse_meta().ok())
        {
            if let Meta::List(list) = meta {
                for meta in list.nested {
                    match meta {
                        NestedMeta::Meta(Meta::Path(path))
                            if mode == ConfigFlagsType::Field && path.is_ident("nested") =>
                        {
                            flags.nested = true
                        }
                        NestedMeta::Meta(Meta::Path(path))
                            if mode == ConfigFlagsType::Field && path.is_ident("toggleable") =>
                        {
                            flags.toggleable = true
                        }
                        NestedMeta::Meta(Meta::Path(path))
                            if mode == ConfigFlagsType::Field && path.is_ident("unstable") =>
                        {
                            flags.unstable = true
                        }
                        NestedMeta::Meta(Meta::NameValue(meta))
                            if mode == ConfigFlagsType::Field && meta.path.is_ident("env") =>
                        {
                            flags.env = Some(EnvFlag(meta.lit))
                        }
                        NestedMeta::Meta(Meta::Path(path))
                            if mode == ConfigFlagsType::Field && path.is_ident("default") =>
                        {
                            flags.default = Some(DefaultFlag::Flag)
                        }
                        NestedMeta::Meta(Meta::NameValue(meta))
                            if mode == ConfigFlagsType::Field && meta.path.is_ident("default") =>
                        {
                            flags.default = Some(DefaultFlag::Value(meta.lit))
                        }
                        NestedMeta::Meta(Meta::NameValue(meta))
                            if mode == ConfigFlagsType::Field && meta.path.is_ident("rename") =>
                        {
                            flags.rename = Some(meta.lit)
                        }
                        NestedMeta::Meta(Meta::NameValue(meta))
                            if mode == ConfigFlagsType::Field
                                && meta.path.is_ident("deprecated") =>
                        {
                            flags.deprecated = Some(meta.lit)
                        }
                        NestedMeta::Meta(Meta::NameValue(meta))
                            if mode == ConfigFlagsType::Container
                                && meta.path.is_ident("map_to") =>
                        {
                            match meta.lit {
                                Lit::Str(val) => {
                                    flags.map_to = Some(Ident::new(&val.value(), Span::call_site()))
                                }
                                _ => {
                                    return Err(meta
                                        .lit
                                        .span()
                                        .error("map_to should be a string literal as value"))
                                }
                            }
                        }
                        NestedMeta::Meta(Meta::NameValue(meta))
                            if mode == ConfigFlagsType::Container
                                && meta.path.is_ident("derive") =>
                        {
                            match meta.lit {
                                Lit::Str(val) => {
                                    flags.derive.extend(
                                        val.value()
                                            .split(',')
                                            .map(|part| Ident::new(part.trim(), Span::call_site())),
                                    );
                                }
                                _ => {
                                    return Err(meta
                                        .lit
                                        .span()
                                        .error("derive should be a string literal as value"))
                                }
                            }
                        }
                        NestedMeta::Meta(Meta::NameValue(meta))
                            if mode == ConfigFlagsType::Container
                                && meta.path.is_ident("generator") =>
                        {
                            match meta.lit {
                                Lit::Str(val) => {
                                    flags.generator =
                                        Some(Ident::new(&val.value(), Span::call_site()))
                                }
                                _ => {
                                    return Err(meta
                                        .lit
                                        .span()
                                        .error("derive should be a string literal as value"))
                                }
                            }
                        }
                        _ => return Err(meta.span().error("unsupported config attribute flag")),
                    }
                }
            }
        }

        Ok(flags)
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
pub enum DefaultFlag {
    // When default has value
    Value(Lit),
    // Just a flag
    Flag,
}

impl ToTokens for DefaultFlag {
    fn to_tokens(&self, tokens: &mut TokenStream) {
        match self {
            DefaultFlag::Value(lit) => {
                let output = match lit {
                    Lit::Bool(_) | Lit::Float(_) | Lit::Int(_) => quote! { .unwrap_or(#lit)},
                    Lit::Str(_) => quote! { .unwrap_or_else(|| #lit.to_string())},
                    _ => unimplemented!("Unsupported default value type"),
                };
                tokens.extend(output);
            }
            DefaultFlag::Flag => {
                tokens.extend(quote! { .unwrap_or_default() });
            }
        }
    }
}
