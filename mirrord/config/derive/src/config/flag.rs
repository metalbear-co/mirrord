use proc_macro2::{Span, TokenStream};
use proc_macro2_diagnostics::{Diagnostic, SpanDiagnosticExt};
use quote::{quote, ToTokens};
use syn::{Attribute, Expr, ExprLit, Ident, Lit, Meta, MetaNameValue};

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

fn lit_or_kaboom(meta: &MetaNameValue) -> Lit {
    if let MetaNameValue {
        path: _,
        eq_token: _,
        value: Expr::Lit(ExprLit { attrs, lit }),
    } = meta
    {
        lit.clone()
    } else {
        panic!("Kaboom `deprecated` meta.lit")
    }
}

impl ConfigFlags {
    pub fn new(attrs: &[Attribute], mode: ConfigFlagsType) -> Result<Self, Diagnostic> {
        /* TODO(alex) [high]: Looks like a stray comma at the end is the issue:

        #[derive(Clone, Debug, Default, serde :: Deserialize,)] <--
        #[serde(deny_unknown_fields)]
         */
        // ADD(alex) [high]: Nope, this doesn't seem to be the issue, I don't see doble
        // commas anywhere (which would be actually breaking).
        let mut flags = ConfigFlags {
            doc: attrs
                .iter()
                .filter(|attr| attr.path().is_ident("doc"))
                .cloned()
                .collect(),
            ..Default::default()
        };

        for meta_list in attrs
            .iter()
            .filter(|attr| attr.path().is_ident("config"))
            .filter_map(|attr| attr.meta.require_list().ok())
        {
            meta_list
                .parse_nested_meta(|nested| {
                    if mode == ConfigFlagsType::Field && nested.path.is_ident("nested") {
                        flags.nested = true
                    } else if mode == ConfigFlagsType::Field && nested.path.is_ident("toggleable") {
                        flags.toggleable = true
                    } else if mode == ConfigFlagsType::Field && nested.path.is_ident("unstable") {
                        flags.unstable = true
                    } else if mode == ConfigFlagsType::Field && nested.path.is_ident("default") {
                        flags.default = Some(DefaultFlag::Flag)
                    } else if let Ok(name_value) = nested.input.parse::<MetaNameValue>() {
                        if mode == ConfigFlagsType::Field && nested.path.is_ident("env") {
                            flags.env = Some(EnvFlag(lit_or_kaboom(&name_value)))
                        } else if mode == ConfigFlagsType::Field && nested.path.is_ident("default")
                        {
                            flags.default = Some(DefaultFlag::Value(lit_or_kaboom(&name_value)))
                        } else if mode == ConfigFlagsType::Field && nested.path.is_ident("rename") {
                            flags.rename = Some(lit_or_kaboom(&name_value))
                        } else if mode == ConfigFlagsType::Field
                            && nested.path.is_ident("deprecated")
                        {
                            flags.deprecated = Some(lit_or_kaboom(&name_value))
                        } else if mode == ConfigFlagsType::Container
                            && nested.path.is_ident("map_to")
                        {
                            let lit = lit_or_kaboom(&name_value);
                            match lit {
                                Lit::Str(val) => {
                                    flags.map_to = Some(Ident::new(&val.value(), Span::call_site()))
                                }
                                _ => {
                                    let fail = lit
                                        .span()
                                        .error("map_to should be a string literal as value");
                                    panic!("{fail:?}");
                                    // return Err(lit
                                    //     .span()
                                    //     .error("map_to should be a string literal as value"));
                                }
                            }
                        } else if mode == ConfigFlagsType::Container
                            && nested.path.is_ident("derive")
                        {
                            let lit = lit_or_kaboom(&name_value);
                            match lit {
                                Lit::Str(val) => {
                                    flags.derive.extend(
                                        val.value()
                                            .split(',')
                                            .map(|part| Ident::new(part.trim(), Span::call_site())),
                                    );
                                }
                                _ => {
                                    let fail = lit
                                        .span()
                                        .error("derive should be a string literal as value");
                                    panic!("{fail:?}");
                                    // return Err(lit
                                    //     .span()
                                    //     .error("derive should be a string literal as value"))
                                }
                            }
                        } else if mode == ConfigFlagsType::Container
                            && nested.path.is_ident("generator")
                        {
                            let lit = lit_or_kaboom(&name_value);
                            match lit {
                                Lit::Str(val) => {
                                    flags.generator =
                                        Some(Ident::new(&val.value(), Span::call_site()))
                                }
                                _ => {
                                    let fail = lit
                                        .span()
                                        .error("derive should be a string literal as value");
                                    panic!("{fail:?}");
                                    // return Err(lit
                                    //     .span()
                                    //     .error("derive should be a string literal as value"))
                                }
                            }
                        }
                    }

                    Ok(())
                })
                .ok();
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
