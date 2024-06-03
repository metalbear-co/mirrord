use proc_macro2::{Span, TokenStream};
use proc_macro2_diagnostics::{Diagnostic, SpanDiagnosticExt};
use quote::{quote, ToTokens};
use syn::{
    punctuated::Punctuated, spanned::Spanned, Attribute, Expr, ExprLit, Ident, Lit, Meta,
    MetaNameValue, Token,
};

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

/// Retrieves the [`Lit`] that is inside a [`MetaNameValue`].
///
/// The [`Lit`] can be:
/// 1. The `CatFile` in `map_to = "CatFile"`;
/// 2. The `JsonSchema` in `derive = "JsonSchema"`;
/// 3. The `CatUserConfig` in `generator = "CatUserConfig"`;
fn lit_in_meta_name_value(meta: &MetaNameValue) -> Option<Lit> {
    if let MetaNameValue {
        path: _,
        eq_token: _,
        value: Expr::Lit(ExprLit { lit, .. }),
    } = meta
    {
        Some(lit.clone())
    } else {
        None
    }
}

impl ConfigFlags {
    pub fn new(attrs: &[Attribute], mode: ConfigFlagsType) -> Result<Self, Diagnostic> {
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
            // The more flexible way of parsing nested `Meta` in a `MetaList` is to use
            // `parse_nested_meta`, but it's also more troublesome, as it parses the meta
            // attribute in a weird way for our usecase.
            // Consider something like
            // `#[config(map_to = "CatFile", derive = "JsonSchema")]`, it would parse this
            // as `Path {map_to, CatFile}` and the rest of the input would be
            // `, derive = "JsonSchema"`, which is a bigger hassle to parse.
            //
            // Also, error handling becomes more problematic with `parse_nested_meta`.
            let nested =
                meta_list.parse_args_with(Punctuated::<Meta, Token![,]>::parse_terminated)?;

            for meta in nested {
                match meta {
                    Meta::Path(path)
                        if mode == ConfigFlagsType::Field && path.is_ident("nested") =>
                    {
                        flags.nested = true;
                    }
                    Meta::Path(path)
                        if mode == ConfigFlagsType::Field && path.is_ident("toggleable") =>
                    {
                        flags.toggleable = true;
                    }
                    Meta::Path(path)
                        if mode == ConfigFlagsType::Field && path.is_ident("unstable") =>
                    {
                        flags.unstable = true;
                    }
                    Meta::NameValue(meta)
                        if mode == ConfigFlagsType::Field && meta.path.is_ident("env") =>
                    {
                        flags.env = lit_in_meta_name_value(&meta).map(EnvFlag);
                    }
                    Meta::Path(path)
                        if mode == ConfigFlagsType::Field && path.is_ident("default") =>
                    {
                        flags.default = Some(DefaultFlag::Flag);
                    }
                    Meta::NameValue(meta)
                        if mode == ConfigFlagsType::Field && meta.path.is_ident("default") =>
                    {
                        flags.default = lit_in_meta_name_value(&meta).map(DefaultFlag::Value);
                    }
                    Meta::NameValue(meta)
                        if mode == ConfigFlagsType::Field && meta.path.is_ident("rename") =>
                    {
                        flags.rename = lit_in_meta_name_value(&meta);
                    }
                    Meta::NameValue(meta)
                        if mode == ConfigFlagsType::Field && meta.path.is_ident("deprecated") =>
                    {
                        flags.deprecated = lit_in_meta_name_value(&meta);
                    }
                    Meta::NameValue(meta)
                        if mode == ConfigFlagsType::Container && meta.path.is_ident("map_to") =>
                    {
                        lit_in_meta_name_value(&meta).map(|lit| match lit {
                            Lit::Str(val) => {
                                flags.map_to = Some(Ident::new(&val.value(), Span::call_site()));
                                Ok(())
                            }
                            _ => Err(lit
                                .span()
                                .error("map_to should be a string literal as value")),
                        });
                    }
                    Meta::NameValue(meta)
                        if mode == ConfigFlagsType::Container && meta.path.is_ident("derive") =>
                    {
                        lit_in_meta_name_value(&meta).map(|lit| match lit {
                            Lit::Str(val) => {
                                flags.derive.extend(
                                    val.value()
                                        .split(',')
                                        .map(|part| Ident::new(part.trim(), Span::call_site())),
                                );
                                Ok(())
                            }
                            _ => Err(lit
                                .span()
                                .error("derive should be a string literal as value")),
                        });
                    }
                    Meta::NameValue(meta)
                        if mode == ConfigFlagsType::Container
                            && meta.path.is_ident("generator") =>
                    {
                        lit_in_meta_name_value(&meta).map(|lit| match lit {
                            Lit::Str(val) => {
                                flags.generator = Some(Ident::new(&val.value(), Span::call_site()));
                                Ok(())
                            }
                            _ => Err(lit
                                .span()
                                .error("derive should be a string literal as value")),
                        });
                    }
                    _ => {
                        return Err(meta.path().span().error(
                            "unsupported config
                         attribute flag",
                        ))
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
