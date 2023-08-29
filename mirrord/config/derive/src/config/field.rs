use proc_macro2_diagnostics::Diagnostic;
use quote::{quote, ToTokens};
use syn::{Field, GenericArgument, Ident, PathArguments, Type, Visibility};

use crate::config::flag::{ConfigFlags, ConfigFlagsType, EnvFlag};

///
//* Representation of a single field
//*
//* ```
//* |---------flags---------|
//*  #[config(env = "TEST")]
//*             |-----ty-----|
//* |vis||ident|      |option|
//*  pub  flag:  Opton<Foobar>,
//* ```
///
#[derive(Debug)]
pub struct ConfigField {
    ident: Option<Ident>,
    option: Option<Type>,
    ty: Type,
    vis: Visibility,
    flags: ConfigFlags,
}

impl ConfigField {
    /// Check if field is `Option<T>` and if so return type of `T`
    fn is_option(field: &Field) -> Option<Type> {
        let seg = if let Type::Path(ty) = &field.ty {
            ty.path.segments.first()
        } else {
            None
        }?;

        (seg.ident == "Option").then(|| match &seg.arguments {
            PathArguments::AngleBracketed(generics) => match generics.args.first() {
                Some(GenericArgument::Type(ty)) => Some(ty.clone()),
                _ => None,
            },
            _ => None,
        })?
    }

    ///
    //* Will create the stuct definition part of the code
    //*
    //* #### 1
    //* ```rust
    //* #[config(env = "TEST")]
    //* pub test: String,
    //* ```
    //* Will output
    //* ```rust
    //* pub test: Option<String>
    //* ```
    //*
    //* #### 2
    //* ```rust
    //* #[config(nested)]
    //* pub test: OtherConfig,
    //* ```
    //* Will output
    //* ```rust
    //* pub test: <OtherConfig as crate::config::FromMirrordConfig>::Generator
    //* ```
    //*
    //* #### 3
    //* ```rust
    //* #[config(rename = "test2")]
    //* pub test: OtherConfig,
    //* ```
    //* Will output
    //* ```rust
    //* #[serde(rename = "test2")]
    //* pub test: Option<String>
    //* ```
    ///
    pub fn definition(&self) -> impl ToTokens {
        let ConfigField {
            ident,
            vis,
            ty,
            option,
            flags,
            ..
        } = &self;

        let docs = flags.doc.iter().map(ToTokens::to_token_stream);

        let ty = option.as_ref().unwrap_or(ty);

        let target = if flags.nested {
            let inner = quote! { <#ty as crate::config::FromMirrordConfig>::Generator };

            if flags.toggleable {
                quote! { crate::util::ToggleableConfig<#inner> }
            } else {
                inner
            }
        } else {
            quote! { #ty }
        };

        let rename = flags
            .rename
            .as_ref()
            .map(|rename| quote! { #[serde(rename = #rename)] });

        quote! {
            #(#docs)*
            #rename
            #vis #ident: Option<#target>
        }
    }

    ///
    //* Will create the actual implementation of the
    //*
    //* #### 1
    //* ```rust
    //* #[config(env = "TEST")]
    //* pub test: String,
    //* ```
    //* Will output
    //* ```rust
    //* test: crate::config::from_env::FromEnv::new("TEST").or(self.test)
    //*           .source_value().transpose()?
    //*           .ok_or(crate::config::ConfigError::ValueNotProvided("MyConfig", "test", Some("TEST")))?
    //* ```
    ///
    pub fn implmentation(&self, parent: &Ident) -> impl ToTokens {
        let ConfigField {
            ident,
            option,
            flags,
            ..
        } = &self;

        // Rest of flow is irrelevant for nested config.
        if flags.nested {
            return quote! { #ident: self.#ident.unwrap_or_default().generate_config(context)? };
        }

        let mut impls = Vec::new();

        if let Some(env) = flags.env.as_ref() {
            impls.push(env.to_token_stream());
        }

        impls.push(quote! { self.#ident });

        let mut layers = Vec::new();

        if let Some(lit) = flags.deprecated.as_ref() {
            layers.push(
                quote! { .layer(|next| crate::config::deprecated::Deprecated::new(#lit, next)) },
            );
        }

        if flags.unstable {
            layers.push(
                quote! { .layer(|next| crate::config::unstable::Unstable::new(stringify!(#parent), stringify!(#ident), next)) }
            );
        }

        let unwrapper = option.is_none().then(|| {
            let env_override = match &flags.env {
                Some(EnvFlag(flag)) => quote! { Some(#flag) },
                None => quote! { None }
            };

            // unwrap to default if exists
            if let Some(default) = flags.default.as_ref() {
                quote! {#default}
            } else {
                quote! { .ok_or(crate::config::ConfigError::ValueNotProvided(stringify!(#parent), stringify!(#ident), #env_override))? }
            }
        });

        let impls = impls
            .into_iter()
            .reduce(|acc, impl_| quote! { #acc.or(#impl_) });

        if layers.is_empty() {
            quote! { #ident: #impls .source_value(context).transpose()?#unwrapper }
        } else {
            quote! { #ident: #impls #(#layers),* .source_value(context).transpose()?#unwrapper }
        }
    }
}

impl TryFrom<Field> for ConfigField {
    type Error = Diagnostic;

    fn try_from(field: Field) -> Result<Self, Self::Error> {
        let flags = ConfigFlags::new(&field.attrs, ConfigFlagsType::Field)?;
        let option = Self::is_option(&field);

        let Field { ident, vis, ty, .. } = field;

        Ok(ConfigField {
            flags,
            ident,
            option,
            ty,
            vis,
        })
    }
}
