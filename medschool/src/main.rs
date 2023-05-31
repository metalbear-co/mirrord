//! `medschool` will extract the docs from the current directory, and build a nice
//! `configuration.md` file.
//!
//! To use it simply run `medschool` on the directory you want to get docs from, for example:
//!
//! ```sh
//! cd rust-project
//! medschool
//! ```
//!
//! It'll look into `rust-project/src` and produce `rust-project/configuration.md`.

#![feature(const_trait_impl)]
#![deny(clippy::missing_docs_in_private_items)]
#![deny(missing_docs)]
use std::{
    collections::{BTreeSet, HashSet},
    fmt::Display,
    fs::{self, File},
    hash::Hash,
    io::Read,
};

use syn::{Attribute, Expr, Ident, Type, TypePath};
use thiserror::Error;

/// We just _eat_ some of these errors (turn them into `None`).
#[derive(Debug, Error)]
enum DocsError {
    /// Error for glob iteration.
    #[error("Glob error {0}")]
    Glob(#[from] glob::GlobError),

    /// Parsing glob pattern.
    #[error("Glob pattern {0}")]
    Pattern(#[from] glob::PatternError),

    /// IO issues we have when reading the source files or producing the `.md` file.
    #[error("IO error {0}")]
    IO(#[from] std::io::Error),
}

/// We're extracting [`syn::Item`] structs and enums into this type.
///
/// Implements [`Ord`] by checking if any of its `PartialType::fields` belongs to another type that
/// we have declared.
#[derive(Debug, Default, Clone)]
struct PartialType {
    /// Only interested in the type name, e.g. `struct {Foo}`.
    ident: String,

    /// The docs of the item, they come in as a bunch of strings from `syn`, and we just hold them
    /// as they come.
    docs: Vec<String>,

    /// Only useful when [`syn::ItemStruct`], we don't look into variants of enums.
    fields: Vec<PartialField>,
}

/// The fields when the item we're looking is [`syn::ItemStruct`].
///
/// Implements [`Ord`] by `PartialField::ty.len()`, thus making primitive types < custom types
/// (usually).
#[derive(Debug, Clone)]
struct PartialField {
    /// The name of the field, e.g. `{foo}: String`.
    ident: String,

    /// The type of the field, e.g. `foo: {String}`.
    ///
    /// When we encounter generics, such as `foo: Option<String>`, we dig into the generics to get
    /// the last [`syn::PathSegment`].
    ///
    /// Keep in mind that the handling for this is very crude, so if you have a field with anything
    /// fancier than just the `foo` example (such as `foo: bar::String`), the assumptions break.
    ty: String,

    /// The docs of the field, they come in as a bunch of strings from `syn`, and we just hold them
    /// as they come.
    ///
    /// These docs will be merged with the type docs of this field's type, if it is a sub-type of
    /// any type that we declared.
    docs: Vec<String>,
}

impl PartialField {
    /// Converts a [`syn::Field`] into [`PartialField`], using
    /// [`get_ident_from_field_skipping_generics`] to get the field type.
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
        self.ty.len().cmp(&other.ty.len())
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
    fn hash<H: ~const std::hash::Hasher>(&self, state: &mut H) {
        self.ident.hash(state)
    }
}

impl Hash for PartialField {
    fn hash<H: ~const std::hash::Hasher>(&self, state: &mut H) {
        self.ident.hash(state)
    }
}

impl Display for PartialType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Prevents crashing the tool if the type is not properly documented.
        let docs = self.docs.last().cloned().unwrap_or_default();
        f.write_str(&docs)?;

        let fields: String = self.fields.iter().map(|field| format!("{field}")).collect();
        f.write_str(&fields)
    }
}

impl Display for PartialField {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Prevents crashing the tool if the field is not properly documented.
        let docs = self.docs.last().cloned().unwrap_or_default();
        f.write_str(&docs)
    }
}

/// Digs into the generics of a field ([`syn::ItemStruct`] only), trying to get the last
/// [`syn::PathArguments`], which (hopefully) contains the concrete type we care about.
///
/// Extracts `Foo` from `foo: Option<Vec<{Foo}>>`.
///
/// Doesn't handle generics of generics though, so if your field is `baz: Option<T>` we're going to
/// be assigning this field type to be the string `"T"` (which is probably not what you wanted).
fn get_ident_from_field_skipping_generics(type_path: TypePath) -> Option<Ident> {
    // welp, this path probably contains generics
    let segment = type_path.path.segments.last()?;

    let mut current_argument = segment.arguments.clone();
    let mut inner_type = None;

    // keep digging into the `PathArguments`
    loop {
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

        break;
    }

    inner_type
}

/// Glues all the `Vec<String>` docs into one big `String`.
///
/// It can also be used to filter out docs with meta comments, such as `${internal}`.
fn pretty_docs(mut docs: Vec<String>) -> String {
    for doc in docs.iter_mut() {
        // removes docs that we don't want in `configuration.md`
        if doc.contains(r"<!--${internal}-->") {
            return "".to_string();
        }

        // `trim` is too aggressive, we just want to remove 1 whitespace
        if doc.starts_with(' ') {
            doc.remove(0);
        }
    }

    [docs.concat(), "\n".to_string()].concat()
}

/// Parses all files in the [`glob::glob`] pattern defined within, in the current directory.
//
// TODO(alex): Support specifying a path.
fn parse_files() -> Result<Vec<syn::File>, DocsError> {
    let paths = glob::glob("./src/**/*.rs")?;

    Ok(paths
        .into_iter()
        .filter_map(Result::ok)
        .by_ref()
        .map(File::open)
        .filter_map(Result::ok)
        .map(|mut file| {
            let mut source = String::with_capacity(30 * 1024);
            let read_amount = file.read_to_string(&mut source)?;
            Ok::<_, DocsError>(String::from(&source[..read_amount]))
        })
        .filter_map(Result::ok)
        .map(|raw_contents| syn::parse_file(&raw_contents))
        .filter_map(Result::ok)
        .collect::<Vec<_>>())
}

/// Look into the [`syn::Attribute`]s of whatever item we're handling, and extract its doc strings.
fn docs_from_attributes(attributes: Vec<Attribute>) -> Vec<String> {
    attributes
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
        .collect()
}

fn main() -> Result<(), DocsError> {
    let type_docs = parse_files()?
        .into_iter()
        // go through each `File` extracting the types into a hierarchical tree based on which types
        // belong to other types
        .flat_map(|syntaxed_file| {
            syntaxed_file
                .items
                .into_iter()
                // convert an `Item` into a `PartialType`
                .filter_map(|item| match item {
                    syn::Item::Mod(item_mod) => {
                        docs_from_attributes(item_mod.attrs);
                        None
                    }
                    syn::Item::Enum(item) => {
                        let thing_docs_untreated = docs_from_attributes(item.attrs);
                        let is_internal = thing_docs_untreated
                            .iter()
                            .any(|doc| doc.contains(r"<!--${internal}-->"));

                        // We only care about types that have docs.
                        if !thing_docs_untreated.is_empty() && !is_internal {
                            Some(PartialType {
                                ident: item.ident.to_string(),
                                docs: thing_docs_untreated,
                                fields: Default::default(),
                            })
                        } else {
                            None
                        }
                    }
                    syn::Item::Struct(item) => {
                        let mut fields = item
                            .fields
                            .into_iter()
                            .filter_map(PartialField::new)
                            .collect::<HashSet<_>>()
                            .into_iter()
                            .collect::<Vec<_>>();
                        fields.sort();

                        let thing_docs_untreated = docs_from_attributes(item.attrs);
                        let is_internal = thing_docs_untreated
                            .iter()
                            .any(|doc| doc.contains(r"<!--${internal}-->"));

                        let public_fields = fields
                            .into_iter()
                            .filter(|field| !field.docs.contains(&r"<!--${internal}-->".into()))
                            .collect::<Vec<_>>();

                        // We only care about types that have docs.
                        if !thing_docs_untreated.is_empty()
                            && !public_fields.is_empty()
                            && !is_internal
                        {
                            Some(PartialType {
                                ident: item.ident.to_string(),
                                docs: thing_docs_untreated,
                                fields: public_fields,
                            })
                        } else {
                            None
                        }
                    }
                    _ => None,
                })
        })
        .collect::<BTreeSet<_>>();

    // The types that have been rebuild after following each field "reference".
    let mut new_types = Vec::with_capacity(8);

    // Perform a depth-first search-ish, building more complete types.
    for type_ in type_docs.iter() {
        let mut new_fields = Vec::with_capacity(8);
        let mut current_fields_iter = type_.fields.iter();

        // Keeps track of where we stopped before digging deeper into some field type, so we can
        // come back to it after we reached an edge.
        let mut previous_position = Vec::new();

        loop {
            // No more fields of this type are part of another type.
            if !type_
                .fields
                .iter()
                .any(|f| type_docs.iter().any(|t| t.ident == f.ty))
            {
                break;
            }

            if let Some(field) = current_fields_iter.next() {
                let new_field = if let Some(child_type) =
                    type_docs.iter().find(|type_| field.ty == type_.ident)
                {
                    // This field belongs to one of our custom types, so we store our iterator
                    // position and change the `current_fields_iter` to dig into this field's
                    // fields.
                    previous_position.push(current_fields_iter.clone());
                    current_fields_iter = child_type.fields.iter();

                    PartialField {
                        ident: field.ident.clone(),
                        ty: field.ty.clone(),
                        // We want to glue the docs that were part of this field in the struct it
                        // belongs to, with the docs of the field's type.
                        docs: [field.docs.clone(), child_type.docs.clone()].concat(),
                    }
                } else {
                    // Primitive type, or type outside our files.
                    field.clone()
                };

                new_fields.push(new_field);
            } else if let Some(previous_iter) = previous_position.pop() {
                // Go back to where we were before digging into this edge.
                current_fields_iter = previous_iter;
            } else {
                // No fields left to see.
                break;
            }
        }

        // Now let's build a type with the mega fields we just built.
        let new_type = PartialType {
            ident: type_.ident.clone(),
            docs: type_.docs.clone(),
            fields: new_fields,
        };
        new_types.push(new_type);
    }

    // If all goes well, the first type should hold the root type.
    let the_mega_type = new_types.first().unwrap().clone();

    let type_docs = pretty_docs(the_mega_type.docs);

    // Concat all the docs!
    let final_docs = [
        type_docs,
        the_mega_type
            .fields
            .into_iter()
            .map(|field| pretty_docs(field.docs))
            .collect::<Vec<_>>()
            .concat(),
    ]
    .concat();

    // TODO(alex): Support specifying a path.
    fs::write("./configuration.md", final_docs).unwrap();

    Ok(())
}

// TODO(alex): Leaving this here as it can be useful for writing tests (and just verifying the
// tool in general).
/*
/// B - 1 line
///
/// B - 2 line
struct B {
    /// ### B - x
    ///
    /// B - x field
    x: i32,

    /// ### B - c
    c: C,
}

/// E - 1 line
///
/// E - 2 line
struct E {
    /// ### E - w
    ///
    /// E - w field
    w: i32,
}

/// # A
///
/// A - 1 line
///
/// A - 2 line
struct A {
    /// ## A - a
    ///
    /// A - a field
    a: i32,

    /// ## A - b
    b: Option<Vec<B>>,

    /// ## A - d
    d: D,

    /// ## A - e
    e: Option<E>,
}


/// C - 1 line
///
/// C - 2 line
struct C {
    /// #### C - y
    ///
    /// C - y field
    y: i32,

    /// #### C - d
    d: D,
}

/// D - 1 line
///
/// D - 2 line
struct D {
    /// #### D - z
    ///
    /// D - z field
    z: i32,
}
*/
