#!/usr/bin/env rust-script
//! This is a script that edits the changelog to be suitable for the website docs. It performs the following changes:
//! 1. Drop everything above towncrier's release notes marker, if there is one
//! 2. Remove links from main headings, which link to the (private) operator repo
//! 3. Remove sections that start "### Internal"
//! 4. Fix empty sections, replace with "No significant changes"
//! 5. Remove links to GitHub issues
//! 6. Insert preamble
//!
//! The preamble fields are read from the environment, so that one action can publish the
//! changelog of more than one repo. See `action.yaml` for the inputs that set them.
//!
//! ```cargo
//! [dependencies]
//! fancy-regex = "0.16"
//! chrono = { version = "0.4.42", features = ["now"] }
//! ```

use std::env;
use std::io::{self, Read};
use fancy_regex::Regex;
use chrono::Utc;

/// Marker towncrier writes above the generated entries. Everything above it is the handwritten
/// header of the source changelog, which has no place in the docs page.
const TOWNCRIER_MARKER: &str = "<!-- towncrier release notes start -->";

fn main() {
    // Read file contents from stdin
    let mut file_contents = String::new();
    io::stdin().read_to_string(&mut file_contents).expect("Failed to read from stdin");

    // Drop the handwritten header above towncrier's marker. Not every changelog has one.
    let file_contents = match file_contents.find(TOWNCRIER_MARKER) {
        Some(index) => file_contents[index + TOWNCRIER_MARKER.len()..].trim_start().to_string(),
        None => file_contents,
    };

    // Remove links from headings
    let heading_regex = Regex::new(r"## \[(.+)\]\(.+\)").unwrap();
    let file_contents = heading_regex.replace_all(&file_contents, "## $1");

    // Remove sections that start "### Internal"
    let internal_regex = Regex::new(r"### Internal[\s\S]*?(?=##)").unwrap();
    let file_contents = internal_regex.replace_all(&file_contents, "");

    // Fix empty sections, replace with "No significant changes"
    let empty_section_regex = Regex::new(r"(## .+)(\n\n\n)(?=## )").unwrap();
    let file_contents = empty_section_regex.replace_all(&file_contents, "$1\n\nNo significant changes.$2");

    // Remove issue links. The id is not always numeric - a towncrier fragment named without the
    // `+` prefix is read as an issue number, and the resulting link points at nothing.
    let issue_link_regex = Regex::new(r"\s*\[#[^\]]*\]\(.*issues.*\)").unwrap();
    let file_contents = issue_link_regex.replace_all(&file_contents, "");

    // Insert preamble
    let file_contents = format!("{}{file_contents}", preamble_with_date().as_str());

    // Print the file contents
    println!("{}", file_contents);
}

pub fn preamble_with_date() -> String {
    format!(
        "---\n\
        title: {}\n\
        date: {}T00:00:00.000Z\n\
        lastmod: {}T00:00:00.000Z\n\
        draft: false\n\
        images: []\n\
        weight: 100\n\
        toc: true\n\
        tags:\n{}\
        description: >-\n  {}\n\
        ---\n\n",
        required("CHANGELOG_TITLE"),
        required("CHANGELOG_DATE"),
        Utc::now().date_naive(),
        tags_block(&required("CHANGELOG_TAGS")),
        required("CHANGELOG_DESCRIPTION"),
    )
}

/// Reads a preamble field, failing loudly if the caller left it out. Publishing a page under
/// another repo's title is worse than not publishing it at all.
fn required(key: &str) -> String {
    match env::var(key) {
        Ok(value) if !value.trim().is_empty() => value,
        _ => panic!("{key} is not set - see the inputs in action.yaml"),
    }
}

/// Turns a comma separated list of tags into the YAML list the preamble expects.
fn tags_block(tags: &str) -> String {
    tags.split(',')
        .map(str::trim)
        .filter(|tag| !tag.is_empty())
        .map(|tag| format!("  - {tag}\n"))
        .collect()
}
