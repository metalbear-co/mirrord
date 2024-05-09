use std::collections::{BTreeSet, HashSet};

use syn::{Attribute, Expr, Ident, Meta};

use crate::{
    types::{PartialField, PartialType, PrettyDocs},
    DocsError,
};

#[tracing::instrument(level = "trace", ret)]
pub fn docs_from_attributes(attributes: Vec<Attribute>) -> Vec<String> {
    attributes
        .into_iter()
        .filter_map(|attribute| {
            if let Meta::NameValue(meta_doc) = attribute.meta {
                if let (ident, Expr::Lit(lit)) = (
                    meta_doc.path.segments.first()?.ident.clone(),
                    meta_doc.value,
                ) {
                    if ident == Ident::new("doc", ident.span()) {
                        if let syn::Lit::Str(lit_str) = lit.lit {
                            let doc = lit_str.value();
                            return Some(if doc.trim().is_empty() {
                                "\n".to_string()
                            } else {
                                format!("{}\n", doc)
                            });
                        }
                    }
                }
            }
            None
        })
        .collect()
}

fn parse_item_mod(item_mod: syn::ItemMod) -> Option<PartialType> {
    let thing_docs_untreated = docs_from_attributes(item_mod.attrs);
    Some(PartialType {
        ident: item_mod.ident.to_string(),
        docs: thing_docs_untreated,
        fields: Default::default(),
    })
}

fn parse_item_enum(item: syn::ItemEnum) -> Option<PartialType> {
    let thing_docs_untreated = docs_from_attributes(item.attrs);
    let is_internal = thing_docs_untreated
        .iter()
        .any(|doc| doc.contains(r"<!--${internal}-->"));

    // We only care about types that have docs.
    (!thing_docs_untreated.is_empty() && !is_internal).then(|| PartialType {
        ident: item.ident.to_string(),
        docs: thing_docs_untreated,
        fields: Default::default(),
    })
}

fn parse_item_struct(item: syn::ItemStruct) -> Option<PartialType> {
    // we used to remove any duplicate fields such as two fields with the same
    // type by converting them to a HashSet
    // for example, struct {a : B, b: B} would duplicate docs for B
    // for our use case this is not necessary, and somehow ends up dropping
    // fields so we're just going to keep the fields as
    // they are and consider this again later
    let fields = item
        .fields
        .into_iter()
        // filter_map -> convert to new PartialField and remove internal fields
        .filter_map(PartialField::new)
        .filter(|field| !field.docs.contains(&r"<!--${internal}-->".into()))
        .map(|mut field| {
            // println!("field: {:?}", field.ident);
            field.pretty_docs();
            field
        })
        .collect::<BTreeSet<_>>();

    let thing_docs_untreated = docs_from_attributes(item.attrs);
    let is_internal = thing_docs_untreated
        .iter()
        .any(|doc| doc.contains(r"<!--${internal}-->"));

    // We only care about types that have docs.
    (!thing_docs_untreated.is_empty() && !fields.is_empty() && !is_internal).then(|| PartialType {
        ident: item.ident.to_string(),
        docs: thing_docs_untreated,
        fields,
    })
}

/// Converts a [`syn::Item`] into a [`PartialType`], if possible.
#[tracing::instrument(level = "trace", ret)]
fn parse_syn_item_into_partial_type(item: syn::Item) -> Option<PartialType> {
    let parsed_item = match item {
        syn::Item::Mod(item_mod) => parse_item_mod(item_mod),
        syn::Item::Enum(item_enum) => parse_item_enum(item_enum),
        syn::Item::Struct(item_struct) => parse_item_struct(item_struct),
        _ => return None,
    };

    parsed_item.map(|mut item| {
        item.pretty_docs();
        item
    })
}

/// Converts a list of [`syn::File`] into a [`BTreeSet`] of our own [`PartialType`] types, so we can
/// get a root node (see the [`Ord`] implementation of `PartialType`).
#[tracing::instrument(level = "trace", ret)]
pub fn parse_docs_into_tree(files: Vec<syn::File>) -> Result<HashSet<PartialType>, DocsError> {
    let type_docs = files
        .into_iter()
        // go through each `File` extracting the types into a hierarchical tree based on which types
        // belong to other types
        .flat_map(|syntaxed_file| {
            syntaxed_file
                .items
                .into_iter()
                // convert an `Item` into a `PartialType`
                .filter_map(parse_syn_item_into_partial_type)
        })
        .collect::<HashSet<_>>();

    Ok(type_docs)
}

/// Gets the element with the most number of [`PartialField`], which at this point should be our
/// root [`PartialType`].
#[tracing::instrument(level = "trace", ret)]
fn get_root_type(types: Vec<PartialType>) -> PartialType {
    types
        .into_iter()
        .max_by(|a, b| a.fields.len().cmp(&b.fields.len()))
        .expect("If we have no elements here, the tool failed!")
}
