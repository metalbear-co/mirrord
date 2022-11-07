use proc_macro2_diagnostics::Diagnostic;
use quote::{quote, ToTokens};
use syn::{Field, GenericArgument, Ident, PathArguments, Type, Visibility};

use crate::flag::{ConfigFlags, ConfigFlagsType, EnvFlag};

#[derive(Debug)]
pub struct FileStructField {
    ident: Option<Ident>,
    option: Option<Type>,
    ty: Type,
    vis: Visibility,
    flags: ConfigFlags,
}

impl FileStructField {
    fn is_option(field: &Field) -> Option<Type> {
        let seg = if let Type::Path(ty) = &field.ty {
            ty.path.segments.first()?
        } else {
            return None;
        };

        if seg.ident != "Option" {
            return None;
        }

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

        quote! {
            #(#docs)*
            #vis #ident: Option<#target>
        }
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

        if flags.nested {
            impls.push(quote! { self.#ident.map(crate::config::MirrordConfig::generate_config).transpose()? })
        } else {
            impls.push(quote! { self.#ident });
        }

        if let Some(default) = flags.default.as_ref() {
            impls.push(default.to_token_stream());
        }

        if flags.from_default {
            impls.push(quote! { crate::config::from_default::FromDefault::default() })
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

impl TryFrom<Field> for FileStructField {
    type Error = Diagnostic;

    fn try_from(field: Field) -> Result<Self, Self::Error> {
        let flags = ConfigFlags::new(&field.attrs, ConfigFlagsType::Field)?;
        let option = Self::is_option(&field);

        let Field { ident, vis, ty, .. } = field;

        Ok(FileStructField {
            flags,
            ident,
            option,
            ty,
            vis,
        })
    }
}
