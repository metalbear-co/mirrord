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
use std::{fs, path::PathBuf};

use file::parse_files;
use parse::{parse_docs_into_tree, produce_docs_from_root_type, resolve_references};
use tracing_subscriber::{fmt::format::FmtSpan, prelude::*};

use crate::error::DocsError;

mod error;
mod file;
mod parse;
mod types;

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
    use std::time::Instant;
    let now = Instant::now();

    let new_types = resolve_references(type_docs);

    let elapsed = now.elapsed();
    println!("Elapsed: {:.2?}", elapsed);

    let mut final_docs = String::new();

    for type_doc in new_types.into_iter() {
        if type_doc.ident == "LayerConfig" {
            final_docs = produce_docs_from_root_type(type_doc);
        }
    }

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
