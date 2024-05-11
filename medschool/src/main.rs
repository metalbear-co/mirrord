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
use std::{fs, fs::File, io::Read, path::PathBuf};

use parse::parse_docs_into_tree;
use tracing_subscriber::{fmt::format::FmtSpan, prelude::*};

use crate::{error::DocsError, parse::resolve_references};

mod error;
mod parse;
mod types;

// TODO(alex): Support specifying a path.
/// Converts all files in the [`glob::glob`] pattern defined within, in the current directory,
/// into a `Vec<String>`.
/// All files are read in parallel to make the best of disk `reads` (assuming SSDs in this case)
/// performance using a threadpool.
#[tracing::instrument(level = "trace", ret)]
pub(crate) fn parse_files(path: PathBuf) -> Result<Vec<syn::File>, DocsError> {
    let paths = glob::glob(&format!("{}/**/*.rs", path.to_string_lossy()))?;

    paths
        .into_iter()
        .filter_map(Result::ok)
        .map(|path| {
            let mut file = File::open(path)?;
            let mut source = String::new();
            file.read_to_string(&mut source)?;

            syn::parse_file(&source).map_err(Into::into)
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

    let MedschoolArgs {
        prepend,
        output,
        input,
    } = <MedschoolArgs as clap::Parser>::parse();

    let files = parse_files(input.unwrap_or_else(|| PathBuf::from("./src")))?;
    let type_docs = parse_docs_into_tree(files)?;
    let resolved = resolve_references(type_docs.clone());

    if let Some(produced) = resolved {
        let mut final_docs = produced.produce_docs();
        match prepend {
            Some(header) => {
                let header = std::fs::read_to_string(header)?;
                final_docs.insert_str(0, &(header + "\n"));
            }
            None => {}
        };

        let output = output.unwrap_or_else(|| PathBuf::from("./configuration.md"));
        fs::write(output, final_docs).unwrap();
    }
    Ok(())
}
