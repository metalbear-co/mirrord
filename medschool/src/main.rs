#![doc = include_str!("../readme.md")]
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
    path::PathBuf,
};

use syn::{Attribute, Expr, Ident, Type, TypePath};
use thiserror::Error;
use tracing_subscriber::{fmt::format::FmtSpan, prelude::*};

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

    /// May happen (probably never) when [`parse_files`] is reading the source file into a `&str`.
    #[error("Read past end of source!")]
    ReadOutOfBounds,
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

/// Digs into the generics of a field ([`syn::ItemStruct`] only), trying to get the last
/// [`syn::PathArguments`], which (hopefully) contains the concrete type we care about.
///
/// Extracts `Foo` from `foo: Option<Vec<{Foo}>>`.
///
/// Doesn't handle generics of generics though, so if your field is `baz: Option<T>` we're going to
/// be assigning this field type to be the string `"T"` (which is probably not what you wanted).
#[tracing::instrument(level = "trace", ret)]
fn get_ident_from_field_skipping_generics(type_path: TypePath) -> Option<Ident> {
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

/// Glues all the `Vec<String>` docs into one big `String`.
///
/// It can also be used to filter out docs with meta comments, such as `${internal}`.
#[tracing::instrument(level = "trace", ret)]
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

// TODO(alex): Support specifying a path.
/// Converts all files in the [`glob::glob`] pattern defined within, in the current directory, into
/// a `Vec<String>`.
#[tracing::instrument(level = "trace", ret)]
fn files_to_string(path: PathBuf) -> Result<Vec<String>, DocsError> {
    let paths = glob::glob(&format!("{}/**/*.rs", path.to_string_lossy()))?;

    Ok(paths
        .into_iter()
        .filter_map(Result::ok)
        .by_ref()
        .map(File::open)
        .filter_map(Result::ok)
        .map(|mut file| {
            let mut source = String::with_capacity(30 * 1024);
            let read_amount = file.read_to_string(&mut source)?;
            let file_read = source
                .get(..read_amount)
                .ok_or(DocsError::ReadOutOfBounds)?;

            Ok::<_, DocsError>(String::from(file_read))
        })
        .filter_map(Result::ok)
        .collect())
}

/// Parses the `files` into a collection of [`syn::File`].
#[tracing::instrument(level = "trace", ret)]
fn parse_string_files(files: Vec<String>) -> Vec<syn::File> {
    files
        .into_iter()
        .map(|raw_contents| syn::parse_file(&raw_contents))
        .filter_map(Result::ok)
        .collect::<Vec<_>>()
}

/// Look into the [`syn::Attribute`]s of whatever item we're handling, and extract its doc strings.
#[tracing::instrument(level = "trace", ret)]
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

/// Converts a list of [`syn::File`] into a [`BTreeSet`] of our own [`PartialType`] types, so we can
/// get a root node (see the [`Ord`] implementation of `PartialType`).
#[tracing::instrument(level = "trace", ret)]
fn parse_docs_into_tree(files: Vec<syn::File>) -> Result<BTreeSet<PartialType>, DocsError> {
    let type_docs = files
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
                        (!thing_docs_untreated.is_empty() && !is_internal).then(|| PartialType {
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
                            .collect::<HashSet<_>>()
                            .into_iter()
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
                        (!thing_docs_untreated.is_empty()
                            && !public_fields.is_empty()
                            && !is_internal)
                            .then(|| PartialType {
                                ident: item.ident.to_string(),
                                docs: thing_docs_untreated,
                                fields: public_fields,
                            })
                    }
                    _ => None,
                })
        })
        .collect::<BTreeSet<_>>();

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
#[tracing::instrument(level = "trace", ret)]
fn depth_first_build_new_types(type_docs: BTreeSet<PartialType>) -> Vec<PartialType> {
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

        // new_fields.sort();
        // Now let's build a type with the mega fields we just built.
        let new_type = PartialType {
            ident: type_.ident.clone(),
            docs: type_.docs.clone(),
            fields: new_fields,
        };
        new_types.push(new_type);
    }

    new_types.sort();

    new_types
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

/// Turns the `root` [`PartialType`] documentation into one big `String`.
#[tracing::instrument(level = "trace", ret)]
fn produce_docs_from_root_type(root: PartialType) -> String {
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

/// Extracts the documentation from Rust source into a markdown file.
#[derive(clap::Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct ToolArgs {
    /// Adds the file contents as a header to the generated markdown.
    #[arg(short, long)]
    prepend: Option<PathBuf>,

    /// Path to the `src` folder you want to generate documentation for.
    ///
    /// Defaults to `./src`.
    #[arg(short, long)]
    input: Option<PathBuf>,

    /// Output file for the generated markdown.
    ///
    /// Defaults to `./configuration.md`.
    #[arg(short, long)]
    output: Option<PathBuf>,
}

/// # Attention when using `RUST_LOG`
///
/// Every function here supports our usual [`tracing::instrument`] setup, with default
/// `log_level = "trace`, but if you dare run with `RUST_LOG=trace` you're going to have a bad time!
///
/// The logging is put in place so you can quickly change whatever function you need to
/// `log_level = "debug"` (or whatever).
///
/// tl;dr: do **NOT** use `RUST_LOG=trace`!
fn main() -> Result<(), DocsError> {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_span_events(FmtSpan::ACTIVE)
                .pretty(),
        )
        .with(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let ToolArgs {
        prepend,
        output,
        input,
    } = <ToolArgs as clap::Parser>::parse();

    let files = files_to_string(input.unwrap_or_else(|| PathBuf::from("./src")))?;
    let files = parse_string_files(files);

    let type_docs = parse_docs_into_tree(files)?;
    let new_types = depth_first_build_new_types(type_docs);

    // If all goes well, the first type should hold the root type.
    let the_mega_type = get_root_type(new_types);

    let final_docs = produce_docs_from_root_type(the_mega_type);

    let final_docs = match prepend {
        Some(header) => {
            let header = std::fs::read_to_string(header)?;
            format!("{header}\n{final_docs}")
        }
        None => final_docs,
    };

    let output = output.unwrap_or_else(|| PathBuf::from("./configuration.md"));
    fs::write(output, final_docs).unwrap();

    Ok(())
}

#[cfg(test)]
mod test {
    #![allow(dead_code)]
    #![allow(non_camel_case_types)]
    use std::path::PathBuf;

    use super::*;

    const EXPECTED: &'static str =
    "# Root\n\nRoot - 1l\n\nRoot - 2l\n\n## Root - root_field\n\nRoot - edge - root_field\n\n## Root - b_field\nRoot_B_Node - 1l\n\nRoot_B_Node - 2l\n\n### Root_B_Node - root_b_node_field\n\nRoot_B_Node - edge - root_b_node_field\n\n### Root_B_Node - c_field\nRoot_B_C_Node - 1l\n\nRoot_B_C_Node - 2l\n\n### Root_B_C_Node - root_b_c_node_field\n\nRoot_B_C_Node - edge - root_b_c_node_field\n\n### Root_B_C_Node - d_field\nRoot_B_C_D_Edge - 1l\n\nRoot_B_C_D_Edge - 2l\n\n#### Root_B_C_D_Edge - c_field\n\nRoot_B_C_D_Edge - edge - c_field\n\n#### Root_B_C_D_Edge - d_field\n\nRoot_B_C_D_Edge - edge - d_field\n\n## Root - e_field\nRoot_E_Edge - 1l\n\nRoot_E_Edge - 2l\n\n## Root_E_Edge - f_field\n\nRoot_E_Edge - edge - f_field\n\n## Root_E_Edge - e_field\n\nRoot_E_Edge - edge - e_field\n\n";

    const FILES: [&'static str; 5] = [
        r#"
    /// Root_B_C_Node - 1l
    ///
    /// Root_B_C_Node - 2l
    struct Root_B_C_Node {
        /// <!--${internal}-->
        /// ### Root_B_C_Node - internal
        internal: i32,

        /// ### Root_B_C_Node - d_field
        d_field: Vec<Root_B_C_D_Edge>,

        /// ### Root_B_C_Node - root_b_c_node_field
        ///
        /// Root_B_C_Node - edge - root_b_c_node_field
        root_b_c_node_field: Option<Vec<String>>,
    }
"#,
        r#"
    /// # Root
    ///
    /// Root - 1l
    ///
    /// Root - 2l
    struct Root {
        /// ## Root - e_field
        e_field: Root_E_Edge,
        
        /// ## Root - root_field
        ///
        /// Root - edge - root_field
        root_field: i32,

        /// ## Root - b_field
        b_field: Option<Vec<Root_B_Node>>,
    }
"#,
        r#"
    /// Root_B_Node - 1l
    ///
    /// Root_B_Node - 2l
    struct Root_B_Node {
        /// ### Root_B_Node - c_field
        c_field: Vec<Root_B_C_Node>,

        /// ### Root_B_Node - root_b_node_field
        ///
        /// Root_B_Node - edge - root_b_node_field
        root_b_node_field: Option<String>,
    }
"#,
        r#"
    /// Root_B_C_D_Edge - 1l
    ///
    /// Root_B_C_D_Edge - 2l
    struct Root_B_C_D_Edge {
        /// #### Root_B_C_D_Edge - d_field
        ///
        /// Root_B_C_D_Edge - edge - d_field
        d_field: Vec<Option<Vec<PathBuf>>>,

        /// #### Root_B_C_D_Edge - c_field
        ///
        /// Root_B_C_D_Edge - edge - c_field
        c_field: i32,
    }
"#,
        r#"
    /// Root_E_Edge - 1l
    ///
    /// Root_E_Edge - 2l
    struct Root_E_Edge {
        /// ## Root_E_Edge - f_field
        ///
        /// Root_E_Edge - edge - f_field
        f_field: i32,

        /// ## Root_E_Edge - e_field
        ///
        /// Root_E_Edge - edge - e_field
        e_field: String,
    }
"#,
    ];

    /// Basic test that determines we're getting the [`EXPECTED`] result, under _ideal-ish_
    /// conditions.
    #[test]
    fn basic_markdown_from_sample_works() {
        let files = parse_string_files(FILES.map(ToString::to_string).to_vec());

        let type_docs = super::parse_docs_into_tree(files).unwrap();
        println!("parsed {type_docs:#?}");

        let new_types = depth_first_build_new_types(type_docs);
        println!("new_types {new_types:#?}");

        let mega_type = get_root_type(new_types);
        println!("mega_type {mega_type:#?}");

        let final_docs = produce_docs_from_root_type(mega_type);
        println!("final_docs {final_docs:#?}");

        assert_eq!(final_docs, EXPECTED);
    }

    /// Randomizes the _file_ order, so we can test that we get [`EXPECTED`] under more realistic
    /// conditions (where we can't force an order for the real files).
    #[test]
    fn randomish_markdown_from_sample_works() {
        let files = parse_string_files(FILES.map(ToString::to_string).to_vec());

        for i in 0..32 {
            let mut files = files.clone();
            files.sort_unstable_by(|_, _| rand::random::<i32>().cmp(&rand::random()));

            let type_docs = super::parse_docs_into_tree(files.clone()).unwrap();
            let new_types = depth_first_build_new_types(type_docs.clone());
            let mega_type = get_root_type(new_types.clone());
            let final_docs = produce_docs_from_root_type(mega_type.clone());

            assert_eq!(
                final_docs, EXPECTED,
                "files{files:#?}\ntype_docs {type_docs:#?}\nnew_types {new_types:#?}\nmega_type{mega_type:#?}\nfina_docs {final_docs:#?}\nindex {i:?}"
            );
        }
    }

    /// # Root_B_C_Node
    ///
    /// Root_B_C_Node - 1l
    ///
    /// Root_B_C_Node - 2l
    struct Root_B_C_Node {
        /// <!--${internal}-->
        /// ## Root_B_C_Node - internal
        internal: i32,

        /// ## Root_B_C_Node - d_field
        d_field: Vec<Root_B_C_D_Edge>,

        /// ## Root_B_C_Node - root_b_c_node_field
        ///
        /// Root_B_C_Node - edge - root_b_c_node_field
        root_b_c_node_field: Option<Vec<String>>,
    }

    /// # Root
    ///
    /// Root - 1l
    ///
    /// Root - 2l
    struct Root {
        /// ## Root - e_field
        e_field: Root_E_Edge,

        /// ## Root - root_field
        ///
        /// Root - edge - root_field
        root_field: i32,

        /// ## Root - b_field
        b_field: Option<Vec<Root_B_Node>>,
    }

    /// # Root_B_Node
    ///
    /// Root_B_Node - 1l
    ///
    /// Root_B_Node - 2l
    struct Root_B_Node {
        /// ## Root_B_Node - c_field
        c_field: Vec<Root_B_C_Node>,

        /// ## Root_B_Node - root_b_node_field
        ///
        /// Root_B_Node - edge - root_b_node_field
        root_b_node_field: Option<String>,
    }

    /// # Root_B_C_D_Edge
    ///
    /// Root_B_C_D_Edge - 1l
    ///
    /// Root_B_C_D_Edge - 2l
    struct Root_B_C_D_Edge {
        /// ## Root_B_C_D_Edge - d_field
        ///
        /// Root_B_C_D_Edge - edge - d_field
        d_field: Vec<Option<Vec<PathBuf>>>,

        /// ## Root_B_C_D_Edge - c_field
        ///
        /// Root_B_C_D_Edge - edge - c_field
        c_field: i32,
    }

    /// # Root_E_Edge
    ///
    /// Root_E_Edge - 1l
    ///
    /// Root_E_Edge - 2l
    struct Root_E_Edge {
        /// ## Root_E_Edge - f_field
        ///
        /// Root_E_Edge - edge - f_field
        f_field: i32,

        /// ## Root_E_Edge - e_field
        ///
        /// Root_E_Edge - edge - e_field
        e_field: String,
    }
}
