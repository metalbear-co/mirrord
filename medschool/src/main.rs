#![feature(const_trait_impl)]
use core::alloc;
use std::{
    collections::{HashMap, HashSet},
    fmt::Display,
    fs::{write, File},
    hash::Hash,
    io::Read,
};

use syn::{spanned::Spanned, Attribute, Expr, Ident, PathSegment, Type, TypePath};
use thiserror::Error;

#[derive(Debug, Clone)]
struct PartialType {
    ident: String,
    docs: Vec<String>,
    fields: HashMap<String, PartialField>,
}

#[derive(Debug, Clone, PartialOrd, Ord)]
struct PartialField {
    ident: String,
    ty: String,
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

// impl std::borrow::Borrow<String> for PartialType {
//     fn borrow(&self) -> &String {
//         &self.ident
//     }
// }

// impl std::borrow::Borrow<String> for PartialField {
//     fn borrow(&self) -> &String {
//         &self.ty
//     }
// }

impl Display for PartialType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.docs.last().unwrap())?;

        let fields: String = self
            .fields
            .iter()
            .map(|(_, field)| format!("{field}"))
            .collect();
        f.write_str(&fields)
    }
}

impl Display for PartialField {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.docs.last().unwrap())
    }
}

fn extract_inner_type_from_generics(segment: PathSegment) -> Option<PathSegment> {
    println!("extracting segment {segment:#?}");
    match segment.arguments {
        syn::PathArguments::None => Some(segment),
        syn::PathArguments::AngleBracketed(bracket_generics) => {
            println!("generics {bracket_generics:#?}");

            bracket_generics
                .args
                .into_iter()
                .last()
                .and_then(|argument| match argument {
                    syn::GenericArgument::Type(t) => {
                        println!("type {t:#?}");

                        match t {
                            Type::Path(type_path) => type_path
                                .path
                                .segments
                                .into_iter()
                                .last()
                                .and_then(extract_inner_type_from_generics),
                            _ => None,
                        }
                    }
                    _ => None,
                })
        }
        syn::PathArguments::Parenthesized(_) => None,
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
        .inspect(|segment| println!("Segment pre \n{segment:#?}\n"))
        // eliminate outer types that we don't want in our docs
        // TODO(alex) [high] 2023-05-26: We don't want to filter the path itself, we want to
        // recursively remove generics until we reach the inner type.
        // .filter(|segment| !ignore_idents.contains(&segment.ident))
        .filter_map(extract_inner_type_from_generics)
        .inspect(|segment| println!("Segment filtered \n{segment:#?}\n"))
        // guarantee that we're done with generics
        // .filter(|segment| segment.arguments.is_empty())
        // .inspect(|segment| println!("Segment no generics \n{segment:#?}\n"))
        .last()
        .map(|segment| segment.ident)
}

fn pretty_docs(docs: &mut Vec<String>) -> String {
    for doc in docs.iter_mut() {
        // removes docs that we don't want in `configuration.md`
        if doc.contains(r"<!--${internal}-->") {
            return "".to_string();
        }
    }

    docs.concat()
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
            ident: field.ident?.to_string(),
            ty: type_ident.to_string(),
            docs: docs_from_attributes(field.attrs),
        })
    }
}

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
        // convert empty lines (spacer lines) into markdown newline, or add `\n` to end of lines
        // (making paragraphs)
        .map(|doc| {
            if doc.trim().len() == 0 {
                "\n".to_string()
            } else {
                format!("{}\n", doc)
            }
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
                            ident: item.ident.to_string(),
                            docs: thing_docs_untreated,
                            fields: Default::default(),
                        })
                    }
                    syn::Item::Struct(item) => {
                        let fields = item
                            .fields
                            .into_iter()
                            .filter_map(PartialField::new)
                            .map(|field| (field.ty.clone(), field))
                            .collect::<HashMap<_, _>>();

                        let thing_docs_untreated = docs_from_attributes(item.attrs);

                        // We only care about types that have docs.
                        (!thing_docs_untreated.is_empty()).then_some(PartialType {
                            ident: item.ident.to_string(),
                            docs: thing_docs_untreated,
                            // fields: Vec::from_iter(fields.into_iter()),
                            fields,
                        })
                    }
                    _ => {
                        // println!("other item");
                        None
                    }
                })
                // use the `PartialType::ident` as a key
                .map(|partial_type| (partial_type.ident.clone(), partial_type))
        })
        // `PartialType`s keyed by the `PartialType::ident`
        // .collect::<HashMap<_, _>>();
        .collect::<HashMap<_, _>>();

    // println!("Untreated \n {type_docs:#?}\n");

    // let mut types_copy: Vec<PartialType> = type_docs.iter().cloned().collect();
    let mut types_copy2 = type_docs.clone();

    // let mut final_types = HashMap::with_capacity(4);

    for (key, type_) in type_docs.iter() {
        for (key2, type2_) in types_copy2.iter_mut() {
            println!("checking if {key:#?} is in {key2:#?}");

            if let Some(type_in_field) = type2_.fields.remove(key) {
                println!("\n\ntype {type_:#?} is in field {type_in_field:#?} of {type2_:#?}");

                let mega_field = PartialField {
                    ident: type_in_field.ident.clone(),
                    ty: type_in_field.ty.clone(),
                    docs: [type_in_field.docs.clone(), type_.docs.clone()].concat(),
                };

                type2_.fields.insert(mega_field.ty.clone(), mega_field);
            }
        }
    }

    println!("TYPES {types_copy2:#?}");

    // for _ in 0..500 {
    //     for current_type in types_copy.iter_mut() {
    //         for (field, mut field_type) in current_type.fields.iter_mut().filter_map(|field| {
    //             type_docs
    //                 .take(&field.ty)
    //                 .and_then(|field_type| Some((field, field_type)))
    //         }) {
    //             field.docs.append(&mut field_type.docs);
    //         }
    //     }
    // }

    // println!("Somewhat treated \n {types_copy:#?}\n");

    // for type_ in types_copy.iter_mut() {
    //     for field in type_.fields.iter_mut() {
    //         field.docs = vec![pretty_docs(&mut field.docs)];
    //     }

    //     type_.docs = vec![pretty_docs(&mut type_.docs)];
    // }

    // println!("Final version \n {types_copy:#?}\n");

    // let test_contents: String = types_copy
    //     .into_iter()
    //     .map(|type_| format!("{}", type_))
    //     .collect();

    // write("./configuration.md", test_contents).unwrap();
    // TODO(alex) [high] 2023-05-23: What's the best way to represent the hierarchy here?
    //
    // Need a way of saying "hey type, are you an inner field of some other type?".

    Ok(())
}

/// # A
///
/// A - 1 line
///
/// A - 2 line
///
/// ```json
/// {
///   "a": 10,
///   "b": "B"
/// }
/// ```
struct A {
    /// ## a
    ///
    /// A - a field
    a: i32,

    /// ## b
    b: B,
    // TODO(alex) [high] 2023-05-26: We're losing generic types, that's why some types end up
    // in places where they shouldn't be (they're not being inlined, as they don't belong to any
    // outer type).
    // ## c
    // c: Option<C>,

    // ## d
    // d: Option<Vec<D>>,
}

/// B - 1 line
///
/// B - 2 line
///
/// ```json
/// {
///   "field": "value"
/// }
/// ```
struct B {
    /// ### x
    ///
    /// B - x field
    x: i32,
    // ### d
    // d: D,
}

/*
/// C - 1 line
///
/// C - 2 line
struct C {
    /// #### y
    ///
    /// C - y field
    y: i32,
}

/// D - 1 line
///
/// D - 2 line
struct D {
    /// #### z
    ///
    /// D - z field
    z: i32,
}
*/
