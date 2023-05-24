#![feature(const_trait_impl)]
use std::{collections::HashSet, fs::File, hash::Hash, io::Read};

use syn::{spanned::Spanned, Attribute, Expr, Ident, Type, TypePath};
use thiserror::Error;

#[derive(Debug)]
struct PartialType {
    ident: Ident,
    docs: Vec<String>,
    fields: HashSet<PartialField>,
}

#[derive(Debug, PartialOrd, Ord)]
struct PartialField {
    ident: Ident,
    ty: Ident,
    docs: Vec<String>,
}

impl PartialEq for PartialType {
    fn eq(&self, other: &Self) -> bool {
        self.ident.eq(&other.ident)
    }
}

impl PartialEq for PartialField {
    fn eq(&self, other: &Self) -> bool {
        self.ident.eq(&other.ident) && self.ty.eq(&other.ty)
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

fn get_ident_from_field_skipping_generics(type_path: TypePath) -> Option<Ident> {
    let span = type_path.span();
    let mut ignore_idents = HashSet::with_capacity(32);
    ignore_idents.insert(Ident::new("Option", span));
    ignore_idents.insert(Ident::new("VecOrSingle", span));
    ignore_idents.insert(Ident::new("Result", span));
    ignore_idents.insert(Ident::new("Vec", span));
    ignore_idents.insert(Ident::new("HashMap", span));
    ignore_idents.insert(Ident::new("HashSet", span));

    // welp, this path probably contains generics
    type_path
        .path
        .segments
        .into_iter()
        // eliminate outer types that we don't want in our docs
        .filter(|segment| !ignore_idents.contains(&segment.ident))
        // guarantee that we're done with generics
        .filter(|segment| segment.arguments.is_empty())
        .last()
        .map(|segment| segment.ident)
}

impl PartialField {
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
            ident: field.ident?,
            ty: type_ident,
            docs: docs_from_attributes(field.attrs),
        })
    }
}

/// Very nice comments!
///
/// But can it handle multiple comments?
///
/// What about multiline comments, that go really far, how does it handle stuff
/// like this?
///
/// ```json
/// {
///   "field": "value"
/// }
#[derive(Debug, Error)]
enum DocsError {
    #[error("Glob error {0}")]
    Glob(#[from] glob::GlobError),

    #[error("Glob error {0}")]
    Pattern(#[from] glob::PatternError),

    #[error("Glob error {0}")]
    IO(#[from] std::io::Error),
}

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
        .collect()
}

fn main() -> Result<(), DocsError> {
    // TODO(alex) [high] 2023-05-22: The plan is:
    //
    // 1. make a `HashMap<Type, (Field { type, docs }, Docs)>` with all the types;
    //
    // Start by making a map of all the types and their docs, we need to include the fields with
    // their types as well, in order to inline the field-type docs later on.
    //
    // 2. search through each Type and check if their Fields belong to another Type;
    //
    // This is the pre-inline process, where we look if the Field { type } belongs in another Type.
    //
    // 3. insert the Field { docs } in the outer Type Docs;
    //
    // Now we inline the docs of Field { docs } in the outer Type Docs.

    let mut type_docs = parse_files()?
        .into_iter()
        // go through each `File` extracting the types into a map keyed by the type `Ident`
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

                        // We only care about types that have docs.
                        (!thing_docs_untreated.is_empty()).then_some(PartialType {
                            ident: item.ident,
                            docs: thing_docs_untreated,
                            fields: HashSet::new(),
                        })
                    }
                    syn::Item::Struct(item) => {
                        let fields = item
                            .fields
                            .into_iter()
                            .filter_map(PartialField::new)
                            .collect::<HashSet<_>>();

                        let thing_docs_untreated = docs_from_attributes(item.attrs);

                        // We only care about types that have docs.
                        (!thing_docs_untreated.is_empty()).then_some(PartialType {
                            ident: item.ident,
                            docs: thing_docs_untreated,
                            fields,
                        })
                    }
                    _ => {
                        println!("other item");
                        None
                    }
                })
            // use the `PartialType::ident` as a key
            // .map(|partial_type| (partial_type.ident.clone(), partial_type))
        })
        // `PartialType`s keyed by the `PartialType::ident`
        // .collect::<HashMap<_, _>>();
        .collect::<HashSet<_>>();

    println!("{type_docs:#?}");

    for type_ in type_docs.iter() {
        // type_.fields.contains()
    }

    // for (_, partial_type) in type_docs.iter() {}

    // let fields = type_docs
    //     .values()
    //     .flat_map(|partial_type| &partial_type.fields)
    //     .collect::<HashSet<_>>();

    // TODO(alex) [high] 2023-05-23: What's the best way to represent the hierarchy here?
    //
    // Need a way of saying "hey type, are you an inner field of some other type?".

    Ok(())
}

/// # A
///
/// Very nice comments!
///
/// This is another line for `A`.
///
/// ```json
/// {
///   "a": 10,
///   "b": "B"
/// }
struct A {
    /// ## a
    ///
    /// a Field .
    a: i32,

    /// ## b
    b: B,
}

/// These are the comments for struct B.
///
/// And it has a second line.
///
/// ```json
/// {
///   "field": "value"
/// }
struct B {
    /// ### c
    ///
    /// It's just a field.
    c: i32,

    /// ### d
    d: D,
}

/// Finally, the comments for D.
///
/// Line!
struct D {
    /// #### e
    ///
    /// It's a field.
    e: i32,
}
