use std::collections::HashMap;

use syn::{Attribute, Expr, Ident, Meta};

use crate::{
    file::pretty_docs,
    types::{PartialField, PartialType},
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


// TODO(mehul): need to shorten this function
#[tracing::instrument(level = "trace", ret)]
fn parse_syn_item_into_partial_type(item: syn::Item) -> Option<PartialType> {
    match item {
        syn::Item::Mod(item_mod) => {
            let thing_docs_untreated = docs_from_attributes(item_mod.attrs);
            Some(PartialType {
                ident: item_mod.ident.to_string(),
                docs: thing_docs_untreated,
                fields: Default::default(),
            })
        }
        syn::Item::Enum(item) => {
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
        syn::Item::Struct(item) => {
            // we used to remove any duplicate fields such as two fields with the same
            // type by converting them to a HashSet
            // for example, struct {a : B, b: B} would duplicate docs for B
            // for our use case this is not necessary, and somehow ends up dropping
            // fields so we're just going to keep the fields as
            // they are and consider this again later
            let fields = item
                .fields
                .into_iter()
                .filter_map(PartialField::new)
                .collect::<Vec<_>>();

            let thing_docs_untreated = docs_from_attributes(item.attrs);
            let is_internal = thing_docs_untreated
                .iter()
                .any(|doc| doc.contains(r"<!--${internal}-->"));

            let mut public_fields = fields
                .into_iter()
                .filter(|field| !field.docs.contains(&r"<!--${internal}-->".into()))
                .collect::<Vec<_>>();

            public_fields.sort();

            // We only care about types that have docs.
            (!thing_docs_untreated.is_empty() && !public_fields.is_empty() && !is_internal).then(
                || PartialType {
                    ident: item.ident.to_string(),
                    docs: thing_docs_untreated,
                    fields: public_fields,
                },
            )
        }
        _ => None,
    }
}

/// Converts a list of [`syn::File`] into a [`BTreeSet`] of our own [`PartialType`] types, so we can
/// get a root node (see the [`Ord`] implementation of `PartialType`).
#[tracing::instrument(level = "trace", ret)]
pub fn parse_docs_into_tree(files: Vec<syn::File>) -> Result<Vec<PartialType>, DocsError> {
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
        .collect::<Vec<_>>();

    Ok(type_docs)
}

/// Digs into the [`PartialTypes`] building new types that inline the types of their
/// [`PartialField`]s, turning something like:
///
/// ```no_run
/// /// A struct
/// struct A {
///     /// x field
///     x: i32,
///     /// b field
///     b: B,
/// }
///
/// /// B struct
/// struct B {
///     /// y field
///     y: i32,
/// }
/// ```
///
/// Into:
///
/// ```no_run
/// /// A struct
/// struct A {
///     /// x field
///     x: i32,
///
///     /// b field
///     /// B struct
///     /// y field
///     y: i32,
/// }
/// ```

/// Resolves the references of the types, so we can inline the docs of the types that are fields of
fn dfs_fields(
    field: &PartialField,
    types: &Vec<PartialType>,
    type_map: &HashMap<String, PartialType>,
) -> String {
    let field_type = type_map.get(&field.ty);

    match field_type {
        Some(t) => {
            let mut new_type_docs = t.docs.clone();
            for field in t.fields.iter() {
                let mut field_docs = field.docs.clone();
                let resolved_type = dfs_fields(field, types, type_map);
                let pretty_field_docs = pretty_docs(field_docs);

                field_docs = vec![pretty_field_docs, resolved_type];

                new_type_docs.push(field_docs.concat());
            }
            pretty_docs(new_type_docs)
        }
        None => "".to_string(),
    }
}

#[tracing::instrument(level = "trace", ret)]
pub fn resolve_references(types: Vec<PartialType>) -> Vec<PartialType> {
    let mut types_clone = types.clone();
    let type_map = types
        .iter()
        .map(|t| (t.ident.clone(), t.clone()))
        .collect::<HashMap<_, _>>();

    for type_ in types_clone.iter_mut() {
        for field in type_.fields.iter_mut() {
            let resolved_type = dfs_fields(field, &types, &type_map);
            let pretty_field_docs = pretty_docs(field.docs.clone());
            field.docs = vec![pretty_field_docs, resolved_type];
        }
    }

    types_clone
}

/// Turns the `root` [`PartialType`] documentation into one big `String`.
#[tracing::instrument(level = "trace", ret)]
pub fn produce_docs_from_root_type(root: PartialType) -> String {
    let type_docs = pretty_docs(root.docs);

    // Concat all the docs!
    [
        type_docs,
        root.fields
            .into_iter()
            .map(|field| pretty_docs(field.docs))
            .collect::<Vec<_>>()
            .concat(),
    ]
    .concat()
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
