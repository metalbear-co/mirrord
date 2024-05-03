use std::{collections::HashMap, fmt::Display, hash::Hash};

use syn::{Attribute, Expr, Ident, Meta, Type, TypePath};

use crate::{convert::pretty_docs, error::DocsError};

/// We're extracting [`syn::Item`] structs and enums into this type.
///
/// Implements [`Ord`] by checking if any of its `PartialType::fields` belongs to another type that
/// we have declared.
#[derive(Debug, Default, Clone)]
pub struct PartialType {
    /// Only interested in the type name, e.g. `struct {Foo}`.
    pub ident: String,

    /// The docs of the item, they come in as a bunch of strings from `syn`, and we just hold them
    /// as they come.
    pub docs: Vec<String>,

    /// Only useful when [`syn::ItemStruct`], we don't look into variants of enums.
    pub fields: Vec<PartialField>,
}

/// The fields when the item we're looking is [`syn::ItemStruct`].
///
/// Implements [`Ord`] by `PartialField::ty.len()`, thus making primitive types < custom types
/// (usually).
#[derive(Debug, Clone)]
pub struct PartialField {
    /// The name of the field, e.g. `{foo}: String`.
    pub ident: String,

    /// The type of the field, e.g. `foo: {String}`.
    ///
    /// When we encounter generics, such as `foo: Option<String>`, we dig into the generics to get
    /// the last [`syn::PathSegment`].
    ///
    /// Keep in mind that the handling for this is very crude, so if you have a field with anything
    /// fancier than just the `foo` example (such as `foo: bar::String`), the assumptions break.
    pub ty: String,

    /// The docs of the field, they come in as a bunch of strings from `syn`, and we just hold them
    /// as they come.
    ///
    /// These docs will be merged with the type docs of this field's type, if it is a sub-type of
    /// any type that we declared.
    pub docs: Vec<String>,
}

impl PartialField {
    /// Converts a [`syn::Field`] into [`PartialField`], using
    /// [`get_ident_from_field_skipping_generics`] to get the field type.
    #[tracing::instrument(level = "trace", ret)]
    fn new(field: syn::Field) -> Option<Self> {
        let type_ident = match field.ty {
            Type::Path(type_path) => {
                // `get_ident` returns `Some` if the `path` doesn't contain generics.
                type_path
                    .path
                    .get_ident()
                    .cloned()
                    .or_else(|| get_ident_from_field_skipping_generics(type_path))
            }
            _ => None,
        }?;

        Some(Self {
            ident: field.ident?.to_string(),
            ty: type_ident.to_string(),
            docs: docs_from_attributes(field.attrs),
        })
    }
}

impl PartialOrd for PartialType {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialOrd for PartialField {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for PartialType {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        if other.fields.iter().any(|field| field.ty == self.ident) {
            // `other` has a field that is of type `self`, so it "belongs" to `self`
            std::cmp::Ordering::Greater
        } else {
            std::cmp::Ordering::Less
        }
    }
}

impl Ord for PartialField {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // primitive types < custom types (usually)
        self.ty
            .len()
            .cmp(&other.ty.len())
            // both field types have the same length, so sort alphabetically
            .then_with(|| self.ident.cmp(&other.ident))
    }
}

impl From<PartialField> for PartialType {
    fn from(value: PartialField) -> Self {
        Self {
            ident: value.ident,
            docs: value.docs,
            fields: Default::default(),
        }
    }
}

impl PartialEq for PartialType {
    fn eq(&self, other: &Self) -> bool {
        self.ident.eq(&other.ident)
    }
}

impl PartialEq for PartialField {
    fn eq(&self, other: &Self) -> bool {
        self.ty.eq(&other.ty)
    }
}

impl Eq for PartialType {}

impl Eq for PartialField {}

impl Hash for PartialType {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.ident.hash(state)
    }
}

impl Hash for PartialField {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.ident.hash(state)
    }
}

impl Display for PartialType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Prevents crashing the tool if the type is not properly documented.
        let docs = self.docs.last().cloned().unwrap_or_default();
        f.write_str(&docs)?;

        self.fields.iter().fold(f, |accum, s| {
            accum.write_str(&s.to_string()).expect("writing failed");
            accum
        });
        Ok(())
    }
}

impl Display for PartialField {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Prevents crashing the tool if the field is not properly documented.
        let docs = self.docs.last().cloned().unwrap_or_default();
        f.write_str(&docs)
    }
}

#[tracing::instrument(level = "trace", ret)]
fn docs_from_attributes(attributes: Vec<Attribute>) -> Vec<String> {
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

/// Digs into the generics of a field ([`syn::ItemStruct`] only), trying to get the last
/// [`syn::PathArguments`], which (hopefully) contains the concrete type we care about.
///
/// Extracts `Foo` from `foo: Option<Vec<{Foo}>>`.
///
/// Doesn't handle generics of generics though, so if your field is `baz: Option<T>` we're going to
/// be assigning this field type to be the string `"T"` (which is probably not what you wanted).
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
