#![feature(const_trait_impl)]
use std::{
    collections::{BTreeSet, HashMap, HashSet},
    fmt::Display,
    fs::{write, File},
    hash::Hash,
    io::Read,
};

use syn::{spanned::Spanned, Attribute, Expr, Ident, Type, TypePath};
use thiserror::Error;

#[derive(Debug, Default, Clone)]
struct PartialType {
    ident: String,
    docs: Vec<String>,
    // fields: HashSet<PartialField>,
    fields: Vec<PartialField>,
}

#[derive(Debug, Clone)]
struct PartialField {
    ident: String,
    ty: String,
    docs: Vec<String>,
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
        // `other` has a field that is of type `self`
        if other.fields.iter().any(|field| field.ty == self.ident) {
            std::cmp::Ordering::Greater
        } else {
            std::cmp::Ordering::Less
        }
    }
}

impl Ord for PartialField {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.ty.cmp(&other.ty)
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
        // self.ident.eq(&other.ident) && self.ty.eq(&other.ty)
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

impl std::borrow::Borrow<String> for PartialType {
    fn borrow(&self) -> &String {
        &self.ident
    }
}

// impl std::borrow::Borrow<String> for PartialField {
//     fn borrow(&self) -> &String {
//         &self.ty
//     }
// }

impl Display for PartialType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.docs.last().unwrap())?;

        let fields: String = self.fields.iter().map(|field| format!("{field}")).collect();
        f.write_str(&fields)
    }
}

impl Display for PartialField {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.docs.last().unwrap())
    }
}

fn get_ident_from_field_skipping_generics(type_path: TypePath) -> Option<Ident> {
    println!("Trying to delve into generics {type_path:#?}");

    let span = type_path.span();
    let mut ignore_idents = HashSet::with_capacity(32);
    ignore_idents.insert(Ident::new("Option", span));
    ignore_idents.insert(Ident::new("VecOrSingle", span));
    ignore_idents.insert(Ident::new("Result", span));
    ignore_idents.insert(Ident::new("Vec", span));
    ignore_idents.insert(Ident::new("HashMap", span));
    ignore_idents.insert(Ident::new("HashSet", span));

    // welp, this path probably contains generics
    let segment = type_path.path.segments.last()?;

    let mut current_argument = segment.arguments.clone();
    let mut inner_type = None;

    loop {
        match &current_argument {
            syn::PathArguments::AngleBracketed(generics) => match generics.args.last()? {
                syn::GenericArgument::Type(t) => match t {
                    Type::Path(generic_path) => {
                        inner_type = match generic_path.path.segments.last() {
                            Some(t) => Some(t.ident.clone()),
                            None => break,
                        };
                        println!("inner {inner_type:#?}");

                        current_argument = generic_path.path.segments.last()?.arguments.clone();
                    }
                    _ => break,
                },
                _ => break,
            },
            _ => break,
        }
    }

    inner_type
}

fn pretty_docs(docs: Vec<String>) -> String {
    for doc in docs.iter() {
        // removes docs that we don't want in `configuration.md`
        if doc.contains(r"<!--${internal}-->") {
            return "".to_string();
        }
    }

    [docs.concat(), "\n".to_string()].concat()
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

/*
fn flatten(list: BTreeSet<PartialType>) -> PartialType {
    let mut list_iter = list.into_iter();
    let root = list_iter.next().unwrap();

    PartialType {
        ident: root.ident.clone(),
        docs: root.docs.clone(),
        fields: expand(&mut list_iter, root.clone()),
    }
}

fn expand(
    all_types: &mut std::collections::btree_set::IntoIter<PartialType>,
    type_: PartialType,
) -> HashSet<PartialField> {
    let fields = type_.fields;
    let mut expanded_fields = HashSet::new();

    for field in fields.iter() {
        if let Some(child) = all_types.find(|type_| type_.ident == field.ty) {
            expanded_fields.extend(expand(all_types, child.clone()));
        } else {
            let new_field = PartialField {
                ident: field.ident.clone(),
                ty: field.ty.clone(),
                docs: [field.docs.clone(), type_.docs.clone()].concat(),
            };
            expanded_fields.insert(new_field);
        }
    }

    expanded_fields
}
*/

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

    let type_docs = parse_files()?
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
                            .collect::<HashSet<_>>();

                        let thing_docs_untreated = docs_from_attributes(item.attrs);

                        // We only care about types that have docs.
                        (!thing_docs_untreated.is_empty()).then_some(PartialType {
                            ident: item.ident.to_string(),
                            docs: thing_docs_untreated,
                            fields: Vec::from_iter(fields.into_iter()),
                            // fields,
                        })
                    }
                    _ => {
                        // println!("other item");
                        None
                    }
                })
            // use the `PartialType::ident` as a key
        })
        .collect::<BTreeSet<_>>();

    println!("types {type_docs:#?}\n");

    let mut new_types = Vec::with_capacity(8);
    for type_ in type_docs.iter() {
        // let mut new_fields = Vec::with_capacity(8);
        let mut new_fields = Vec::with_capacity(8);
        let mut current_fields_iter = type_.fields.iter();

        let mut previous_position = Vec::new();

        loop {
            if let Some(field) = current_fields_iter.next() {
                let new_field = if let Some(child_type) =
                    type_docs.iter().find(|type_| field.ty == type_.ident)
                // .get(&field.ty)
                {
                    previous_position.push(current_fields_iter.clone());
                    current_fields_iter = child_type.fields.iter();

                    println!("field {field:?} | child_type {child_type:?}");

                    PartialField {
                        ident: field.ident.clone(),
                        ty: field.ty.clone(),
                        docs: [field.docs.clone(), child_type.docs.clone()].concat(),
                    }
                } else {
                    field.clone()
                };

                println!("new_field {new_field:?}");

                new_fields.push(new_field);
                // new_fields.insert(new_field);
            } else if let Some(previous_iter) = previous_position.pop() {
                println!("previous {previous_iter:?} \ncurrent {current_fields_iter:?}");
                current_fields_iter = previous_iter;
            } else {
                break;
            }
        }

        let new_type = PartialType {
            ident: type_.ident.clone(),
            docs: type_.docs.clone(),
            fields: new_fields,
        };
        new_types.push(new_type);
    }

    // let the_mega_type = flatten(type_docs.clone());
    // println!("mehul's type \n{the_mega_type:#?}");

    println!("new_types \n{new_types:#?}");

    let the_mega_type = new_types.first().unwrap().clone();

    let type_docs = pretty_docs(the_mega_type.docs);
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

    println!("final docs \n{final_docs:#?}\n");

    write("./configuration.md", final_docs).unwrap();

    Ok(())
}

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
    // ### B - f
    // f: F,
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
    // TODO(alex) [high] 2023-05-26: We're losing generic types, that's why some types end up
    // in places where they shouldn't be (they're not being inlined, as they don't belong to any
    // outer type).
    /// ## A - e
    e: Option<E>,
}

// F - 1 line
//
// F - 2 line
// struct F {
// ### F - k
//
// F - k field
// k: i32,
// }

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
