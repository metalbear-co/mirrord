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
#![feature(iterator_try_collect)]
#![deny(clippy::missing_docs_in_private_items)]
#![deny(missing_docs)]

use std::{fs, fs::File, io::Read, path::PathBuf};

use parse::parse_docs_into_set;
use tracing_subscriber::{fmt::format::FmtSpan, prelude::*};

use crate::{error::DocsError, parse::resolve_references};

mod error;
mod parse;
mod types;

// TODO(alex): Support specifying a path.
/// Converts all files in the [`glob::glob`] pattern defined within, in the current directory,
/// into a `Result<Vec<syn::File>, DocsError>`.
#[tracing::instrument(level = "trace", ret)]
pub(crate) fn parse_files(path: PathBuf) -> Result<Vec<syn::File>, DocsError> {
    let mut paths = glob::glob(&format!("{}/**/*.rs", path.to_string_lossy()))?.peekable();
    paths.peek().ok_or_else(|| DocsError::NoFiles)?;

    paths
        .map(|path| {
            let mut file = File::open(path?)?;
            let mut contents = String::new();
            file.read_to_string(&mut contents)?;
            syn::parse_file(&contents).map_err(DocsError::Parse)
        })
        .collect()
}

/// Extracts the documentation from Rust source into a markdown file.
#[derive(clap::Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct MedschoolArgs {
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
/// Every function here supports our usual [`mod@tracing::instrument`] setup, with default
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

    let MedschoolArgs {
        prepend,
        output,
        input,
    } = <MedschoolArgs as clap::Parser>::parse();

    let files = parse_files(input.unwrap_or_else(|| PathBuf::from("./src")))?;
    let type_docs = parse_docs_into_set(files)?;
    let resolved = resolve_references(type_docs.clone());

    if let Some(produced) = resolved {
        let mut final_docs = produced.produce_docs();
        if let Some(header) = prepend {
            let header = fs::read_to_string(header)?;
            final_docs.insert_str(0, &(header + "\n"));
        }
        let output = output.unwrap_or_else(|| PathBuf::from("./configuration.md"));
        fs::write(output, final_docs).unwrap();
    }
    Ok(())
}

#[cfg(test)]
mod test {
    #![allow(dead_code)]
    #![allow(non_camel_case_types)]

    use super::*;

    const EXPECTED: &str =
    "# Root\n\nRoot - 1l\n\nRoot - 2l\n\n## Root - b_field\n\nRoot_B_Node - 1l\n\nRoot_B_Node - 2l\n\n### Root_B_Node - c_field\n\nRoot_B_C_Node - 1l\n\nRoot_B_C_Node - 2l\n\n### Root_B_C_Node - d_field\n\nRoot_B_C_D_Edge - 1l\n\nRoot_B_C_D_Edge - 2l\n\n#### Root_B_C_D_Edge - c_field\n\nRoot_B_C_D_Edge - edge - c_field\n\n#### Root_B_C_D_Edge - d_field\n\nRoot_B_C_D_Edge - edge - d_field\n\n### Root_B_C_Node - root_b_c_node_field\n\nRoot_B_C_Node - edge - root_b_c_node_field\n\n### Root_B_Node - root_b_node_field\n\nRoot_B_Node - edge - root_b_node_field\n\n## Root - e_field\n\nRoot_E_Edge - 1l\n\nRoot_E_Edge - 2l\n\n## Root_E_Edge - e_field\n\nRoot_E_Edge - edge - e_field\n\n## Root_E_Edge - f_field\n\nRoot_E_Edge - edge - f_field\n\n## Root - root_field\n\nRoot - edge - root_field\n\n";

    const FILES: [&str; 5] = [
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

    fn parse_string_files(files: Vec<String>) -> Vec<syn::File> {
        files
            .into_iter()
            .map(|file| syn::parse_file(&file).unwrap())
            .collect()
    }

    /// Basic test that determines we're getting the [`EXPECTED`] result, under _ideal-ish_
    /// conditions.
    #[test]
    fn basic_markdown_from_sample_works() {
        let files = parse_string_files(FILES.map(ToString::to_string).to_vec());

        let type_docs = super::parse_docs_into_set(files).unwrap();
        println!("parsed {type_docs:#?}");

        let root_type = resolve_references(type_docs.clone()).unwrap();

        let final_docs = root_type.produce_docs();
        println!("final_docs {final_docs:#?}");

        assert_eq!(final_docs, EXPECTED);
    }
    /// Randomizes the _file_ order, so we can test that we get [`EXPECTED`] under more realistic
    /// conditions (where we can't force an order for the real files).
    #[test]
    fn randomish_markdown_from_sample_works() {
        let files = parse_string_files(FILES.map(ToString::to_string).to_vec());

        for _ in 0..32 {
            let mut files = files.clone();
            files.sort_unstable_by(|_, _| rand::random::<i32>().cmp(&rand::random()));

            let type_docs = super::parse_docs_into_set(files).unwrap();
            let root_type = resolve_references(type_docs.clone()).unwrap();
            let final_docs = root_type.produce_docs();

            assert_eq!(final_docs, EXPECTED);
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
