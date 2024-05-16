//! All parsing of `syn::File`` and `syn::Item`` into relevant types and fields related functions.

use std::collections::{BTreeSet, HashMap, HashSet};

use syn::{Attribute, Expr, Ident, Meta, Type, TypePath};

use crate::{
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

/// Digs into the generics of a field ([`syn::ItemStruct`] only), trying to get the last
/// [`syn::PathArguments`], which (hopefully) contains the concrete type we care about.
///
/// Extracts `Foo` from `foo: Option<Vec<{Foo}>>`.
///
/// Doesn't handle generics of generics though, so if your field is `baz: Option<T>` we're going
/// to be assigning this field type to be the string `"T"` (which is probably not what you
/// wanted).
#[tracing::instrument(level = "trace", ret)]
pub fn get_ident_from_field_skipping_generics(type_path: TypePath) -> Option<Ident> {
    // welp, this path probably contains generics
    let segment = type_path.path.segments.last()?;

    let mut current_argument = segment.arguments.clone();
    let mut inner_type = None;

    // keep digging into the `PathArguments`
    while let syn::PathArguments::AngleBracketed(generics) = &current_argument {
        // go directly to the last piece of generics, skipping lifetimes
        match generics.args.last()? {
            // finally have something that resembles a type, but might be an `Option`, so
            // we have to go deeper!
            syn::GenericArgument::Type(Type::Path(generic_path)) => {
                // that's it, we've reached the final type
                inner_type = match generic_path.path.segments.last() {
                    Some(t) => Some(t.ident.clone()),
                    None => break,
                };

                current_argument = generic_path.path.segments.last()?.arguments.clone();
            }
            _ => break,
        }
    }

    inner_type
}

/// Converts a [`syn::ItemMod`] into a [`PartialType`].
fn parse_item_mod(item_mod: syn::ItemMod) -> Option<PartialType> {
    let thing_docs_untreated = docs_from_attributes(item_mod.attrs);
    Some(PartialType {
        ident: item_mod.ident.to_string(),
        docs: thing_docs_untreated,
        fields: Default::default(),
    })
}

/// Converts a [`syn::ItemEnum`] into a [`PartialType`].
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

/// Converts a [`syn::ItemStruct`] into a [`PartialType`].
fn parse_item_struct(item: syn::ItemStruct) -> Option<PartialType> {
    let mut docs = docs_from_attributes(item.attrs);

    if docs.is_empty() {
        return None;
    }

    for doc in docs.iter_mut() {
        // removes docs that we don't want in `configuration.md`
        if doc.contains(r"<!--${internal}-->") {
            return None;
        }

        // `trim` is too aggressive, we just want to remove 1 whitespace
        if doc.starts_with(' ') {
            doc.remove(0);
        }
    }

    docs.push("\n".to_string());

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
        .collect::<BTreeSet<_>>();

    // We only care about types that have docs.
    (!fields.is_empty()).then(|| PartialType {
        ident: item.ident.to_string(),
        docs,
        fields,
    })
}

/// Converts a [`syn::Item`] into a [`PartialType`], if possible.
#[tracing::instrument(level = "trace", ret)]
fn parse_syn_item_into_partial_type(item: syn::Item) -> Option<PartialType> {
    match item {
        syn::Item::Mod(item_mod) => parse_item_mod(item_mod),
        syn::Item::Enum(item_enum) => parse_item_enum(item_enum),
        syn::Item::Struct(item_struct) => parse_item_struct(item_struct),
        _ => return None,
    }
}

/// Converts a list of [`syn::File`] into a [`BTreeSet`] of our own [`PartialType`] types, so we can
/// get a root node (see the [`Ord`] implementation of `PartialType`).
#[tracing::instrument(level = "trace", ret)]
pub fn parse_docs_into_set(files: Vec<syn::File>) -> Result<HashSet<PartialType>, DocsError> {
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

/// DFS helper function to resolve the references of the types. Returns the docs of the fields of
/// the field we're currently looking at search till its leaf nodes. The leaf here means a primitive
/// type for which we don't have a [`PartialType`].
fn dfs_fields(
    field: &PartialField,
    types: &HashSet<PartialType>,
    cache: &mut HashMap<String, Vec<String>>,
    recursion_level: &mut usize,
) -> Vec<String> {
    // increment the recursion level as we're going deeper into the tree
    types // get the type of the field from the types set to recurse into it's fields
        .get(&field.ty)
        .map(|type_| {
            cache.get(&type_.ident).cloned().unwrap_or_else(|| {
                // check if we've already resolved the type
                let mut max_recursion_level = 0;
                let mut new_type_docs = type_.docs.clone();
                type_.fields.iter().for_each(|field| {
                    let mut current_recursion_level = *recursion_level + 1;
                    let resolved_type_docs =
                        dfs_fields(field, types, cache, &mut current_recursion_level);
                    max_recursion_level = max_recursion_level.max(current_recursion_level);
                    cache.insert(field.ty.clone(), resolved_type_docs.clone());
                    // append the docs of the field to the resolved type docs
                    new_type_docs.extend(field.docs.clone().into_iter().chain(resolved_type_docs));
                });
                cache.insert(type_.ident.clone(), new_type_docs.clone());
                *recursion_level = max_recursion_level;
                new_type_docs
            })
        })
        .unwrap_or_default()
}

/// Resolves the references of the types, so we can inline the docs of the types that are fields of
/// other types. Following a DFS approach to resolve the references with memoization it
/// digs into the [`PartialTypes`] building new types that inline the types of their
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
/// Returns the element with the maximum recursion, which at this point should be our
/// root [`PartialType`]. This is just an assumption and an alternate implementation could
/// be where we resolve all references and return the same HashSet and let the caller
/// decide what the root should be.
#[tracing::instrument(level = "trace", ret)]
pub fn resolve_references(types: HashSet<PartialType>) -> Option<PartialType> {
    // Cache to perform memoization between recursive calls so we don't have to resolve the same
    // type multiple times. Mapping between `ident` -> `resolved_docs`.
    // For example, if we have a types [`A`, `B`, `C`] and A has a field of type `B` and `B` has a
    // field of type `C`, and `C` has already been resolved, we don't want to resolve `C` again
    // as we iterate over the types. A -> (B -> C), (B -> C), (C)
    let mut cache = HashMap::with_capacity(types.len());

    types
        .clone()
        .into_iter()
        .flat_map(|mut type_| {
            // Check if the type has already been resolved.
            (!cache.contains_key(&type_.ident)).then(|| {
                // We need to calculate the recursion level for the type, so we can get the root
                // type later on.
                let mut recursion_level = 0;
                // Resolve the references of the fields of the type and modify the type.
                type_.fields = type_
                    .fields
                    .into_iter()
                    .map(|mut field| {
                        // Depth first search to resolve the references of the fields with the types
                        // as our lookup table.
                        let resolved_type_docs =
                            dfs_fields(&field, &types, &mut cache, &mut recursion_level);
                        // append the docs of the field to the resolved type docs
                        field.docs.extend(resolved_type_docs);
                        field
                    })
                    .collect::<BTreeSet<_>>();
                (recursion_level, type_)
            })
        })
        // Get the type with the maximum recursion level, which should be our root type.
        .max_by_key(|(recursion_level, _)| *recursion_level)
        .map(|(_, type_)| type_)
}
