use proc_macro2::{Span, TokenStream};
use proc_macro2_diagnostics::{Diagnostic, SpanDiagnosticExt};
use quote::{quote, ToTokens};
use syn::{
    spanned::Spanned, Data, DataStruct, DeriveInput, Fields, FieldsNamed, Ident, Visibility,
};

use crate::{
    field::FileStructField,
    flag::{ConfigFlags, ConfigFlagsType},
};

#[derive(Debug)]
pub struct FileStruct {
    vis: Visibility,
    ident: Ident,
    fields: Vec<FileStructField>,
    source: Ident,
    flags: ConfigFlags,
}

impl FileStruct {
    pub fn new(input: DeriveInput) -> Result<Self, Diagnostic> {
        let DeriveInput {
            attrs,
            data,
            ident: source,
            vis,
            ..
        } = input;

        let flags = ConfigFlags::new(&attrs, ConfigFlagsType::Container)?;

        let fields = match data {
            Data::Struct(DataStruct { fields, .. }) => match fields {
                Fields::Named(FieldsNamed { named, .. }) => named
                    .into_iter()
                    .map(FileStructField::try_from)
                    .collect::<Result<_, _>>()?,
                _ => return Err(fields.span().error("Unnamed Structs are not supported")),
            },
            _ => return Err(source.span().error("Enums and Unions are not supported")),
        };

        let ident = flags
            .map_to
            .clone()
            .unwrap_or_else(|| Ident::new(&format!("File{}", &source), Span::call_site()));

        Ok(FileStruct {
            vis,
            source,
            ident,
            fields,
            flags,
        })
    }
}

impl ToTokens for FileStruct {
    fn to_tokens(&self, tokens: &mut TokenStream) {
        let FileStruct {
            ident,
            vis,
            fields,
            source,
            flags: ConfigFlags { doc, derive, .. },
        } = &self;

        let field_definitions = fields.iter().map(|field| field.definition());
        let field_impl = fields.iter().map(|field| field.implmentation(source));

        tokens.extend(quote! {
            #[derive(Debug, Clone, serde::Deserialize, #(#derive),*)]
            #[serde(deny_unknown_fields)]
            #(#doc)*
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
