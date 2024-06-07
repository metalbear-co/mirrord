//! All parsing of `syn::File`` and `syn::Item`` into relevant types and fields related functions.

use std::collections::{BTreeSet, HashMap, HashSet};

use syn::{Attribute, Expr, Ident, Type, TypePath};

use crate::{
    types::{PartialField, PartialType},
    DocsError,
};

/// Look into the [`syn::Attribute`]s of whatever item we're handling, and extract its doc strings.
#[tracing::instrument(level = "trace", ret)]
pub fn docs_from_attributes(attributes: Vec<Attribute>) -> Option<Vec<String>> {
    let mut docs: Vec<String> = attributes
        .into_iter()
        // drill into `Meta::NameValue`
        .filter_map(|attribute| match attribute.meta {
            syn::Meta::NameValue(meta_doc) => Some(meta_doc),
            _ => None,
        })
        // retrieve only when `PathSegment::Ident` is "doc"
        .filter_map(|meta_doc| {
            let ident = meta_doc.path.segments.first()?.ident.clone();

            if ident == Ident::new("doc", ident.span()) {
                Some(meta_doc.value)
            } else {
                None
            }
        })
        // get the doc `Expr::Lit`
        .filter_map(|docs| match docs {
            Expr::Lit(doc_expr) => Some(doc_expr.lit),
            _ => None,
        })
        .filter_map(|doc_lit| match doc_lit {
            syn::Lit::Str(doc_lit) => Some(doc_lit.value()),
            _ => None,
        })
        // convert empty lines (spacer lines) into markdown newline, or add `\n` to end of lines,
        // making paragraphs
        .map(|doc| {
            if doc.trim().is_empty() {
                "\n".to_string()
            } else {
                format!("{}\n", doc)
            }
        })
        .collect();

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

    if docs.is_empty() {
        return None;
    }

    docs.push("\n".to_string());
    Some(docs)
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
impl TryFrom<syn::ItemMod> for PartialType {
    type Error = ();

    fn try_from(item: syn::ItemMod) -> Result<Self, Self::Error> {
        Ok(PartialType {
            ident: item.ident.to_string(),
            docs: docs_from_attributes(item.attrs).ok_or(())?,
            fields: Default::default(),
        })
    }
}

/// Converts a [`syn::ItemEnum`] into a [`PartialType`].
/// Currently, we don't handle enum variants as fields, we just use its top-level docs.
impl TryFrom<syn::ItemEnum> for PartialType {
    type Error = ();

    fn try_from(item: syn::ItemEnum) -> Result<Self, Self::Error> {
        Ok(PartialType {
            ident: item.ident.to_string(),
            docs: docs_from_attributes(item.attrs).ok_or(())?,
            fields: Default::default(),
        })
    }
}

/// Converts a [`syn::ItemStruct`] into a [`PartialType`].
/// We remove docs that have `<!--${internal}-->` (they're not converted to [`PartialType`]s).
impl TryFrom<syn::ItemStruct> for PartialType {
    type Error = ();

    fn try_from(item: syn::ItemStruct) -> Result<Self, Self::Error> {
        let fields = item
            .fields
            .into_iter()
            .filter_map(|field| PartialField::try_from(field).ok())
            .collect::<BTreeSet<_>>();

        Ok(PartialType {
            ident: item.ident.to_string(),
            docs: docs_from_attributes(item.attrs).ok_or(())?,
            fields,
        })
    }
}

/// Converts a [`syn::Item`] into a [`PartialType`], if possible.
impl TryFrom<syn::Item> for PartialType {
    type Error = ();

    fn try_from(item: syn::Item) -> Result<Self, Self::Error> {
        match item {
            syn::Item::Struct(item_struct) => PartialType::try_from(item_struct),
            syn::Item::Enum(item_enum) => PartialType::try_from(item_enum),
            syn::Item::Mod(item_mod) => PartialType::try_from(item_mod),
            _ => Err(()),
        }
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
                .filter_map(|item| PartialType::try_from(item).ok())
        })
        .collect::<HashSet<_>>();

    Ok(type_docs)
}

/// DFS helper function to resolve the references of the types. Returns the docs of the fields of
/// the field we're currently looking at search till its leaf nodes. The leaf here means a primitive
/// type for which we don't have a [`PartialType`].
fn dfs_fields<'a>(
    field: &PartialField,
    types: &'a HashSet<PartialType>,
    cache: &mut HashMap<&'a str, Vec<String>>,
    recursion_level: &mut usize,
) -> Vec<String> {
    // increment the recursion level as we're going deeper into the tree
    types // get the type of the field from the types set to recurse into it's fields
        .get(&field.ty)
        .map(|type_| {
            cache.get(&type_.ident as &str).cloned().unwrap_or_else(|| {
                // check if we've already resolved the type
                let mut max_recursion_level = 0;
                let mut new_type_docs = type_.docs.clone();
                type_.fields.iter().for_each(|field| {
                    let mut current_recursion_level = *recursion_level + 1;
                    let resolved_type_docs =
                        dfs_fields(field, types, cache, &mut current_recursion_level);
                    max_recursion_level = max_recursion_level.max(current_recursion_level);
                    cache.insert(&field.ty, resolved_type_docs.clone());
                    // append the docs of the field to the resolved type docs
                    new_type_docs.extend(field.docs.clone().into_iter().chain(resolved_type_docs));
                });
                cache.insert(&type_.ident, new_type_docs.clone());
                *recursion_level = max_recursion_level;
                new_type_docs
            })
        })
        .unwrap_or_default()
}

/// Resolves the references of the types, so we can inline the docs of the types that are fields of
/// other types. Following a DFS approach to resolve the references with memoization it
/// digs into the [`PartialType`]s building new types that inline the types of their
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
            (!cache.contains_key(&type_.ident as &str)).then(|| {
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
