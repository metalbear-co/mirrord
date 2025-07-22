use proc_macro2::{Span, TokenStream};
use proc_macro2_diagnostics::{Diagnostic, SpanDiagnosticExt};
use quote::{quote, ToTokens};
use syn::{Data, DataStruct, DeriveInput, Fields, FieldsNamed, Ident, Visibility};

mod field;
mod flag;

use crate::config::{
    field::ConfigField,
    flag::{ConfigFlags, ConfigFlagsType},
};

#[derive(Debug)]
pub struct ConfigStruct {
    vis: Visibility,
    ident: Ident,
    fields: Vec<ConfigField>,
    source: Ident,
    flags: ConfigFlags,
}

impl ConfigStruct {
    pub fn new(input: DeriveInput) -> Result<Self, Diagnostic> {
        let DeriveInput {
            attrs,
            data,
            ident: source,
            vis,
            ..
        } = input;

        let flags = ConfigFlags::new(&attrs, ConfigFlagsType::Container)?;

        let fields = if let Data::Struct(DataStruct {
            fields: Fields::Named(FieldsNamed { named, .. }),
            ..
        }) = data
        {
            Ok(named
                .into_iter()
                .map(ConfigField::try_from)
                .collect::<Result<_, _>>()?)
        } else {
            Err(source
                .span()
                .error("Enums, Unions, and Unnamed Structs are not supported"))
        }?;

        let ident = flags
            .map_to
            .clone()
            .unwrap_or_else(|| Ident::new(&format!("File{}", &source), Span::call_site()));

        Ok(ConfigStruct {
            vis,
            source,
            ident,
            fields,
            flags,
        })
    }
}

impl ToTokens for ConfigStruct {
    fn to_tokens(&self, tokens: &mut TokenStream) {
        let ConfigStruct {
            ident,
            vis,
            fields,
            source,
            flags:
                ConfigFlags {
                    doc,
                    derive,
                    generator,
                    ..
                },
        } = &self;

        let field_definitions = fields.iter().map(|field| field.definition());
        let field_impl = fields.iter().map(|field| field.implmentation(source));

        let generator = generator.as_ref().unwrap_or(ident);

        tokens.extend(quote! {
            #[derive(Clone, Debug, Default, serde::Deserialize, #(#derive),*)]
            #[serde(deny_unknown_fields)]
            #(#doc)*
            #vis struct #ident { #(#field_definitions),* }

            impl crate::config::MirrordConfig for #ident {
                type Generated = #source;

                fn generate_config(self, context: &mut crate::config::ConfigContext) -> crate::config::Result<Self::Generated> {
                    Ok(#source {
                        #(#field_impl),*
                    })
                }
            }

            impl crate::config::FromMirrordConfig for #source {
                type Generator = #generator;
            }
        });
    }
}
