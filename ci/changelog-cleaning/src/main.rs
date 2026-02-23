#!/usr/bin/env rust-script
//! This is a script that edits the changelog to be suitable for the website docs. It performs the following changes:
//! 1. Remove links from main headings, which link to the (private) operator repo
//! 2. Remove sections that start "### Internal"
//! 3. Fix empty sections, replace with "No significant changes"
//! 4. Remove links to GitHub issues
//! 5. Insert preamble
//!
//! ```cargo
//! [dependencies]
//! fancy-regex = "0.16"
//! chrono = { version = "0.4.42", features = ["now"] }
//! ```

use std::io::{self, Read};
use fancy_regex::Regex;
use chrono::Utc;

fn main() {
    // Read file contents from stdin
    let mut file_contents = String::new();
    io::stdin().read_to_string(&mut file_contents).expect("Failed to read from stdin");

    // Remove links from headings
    let heading_regex = Regex::new(r"## \[(.+)\]\(.+\)").unwrap();
    let file_contents = heading_regex.replace_all(&file_contents, "## $1");

    // Remove sections that start "### Internal"
    let internal_regex = Regex::new(r"### Internal[\s\S]*?(?=##)").unwrap();
    let file_contents = internal_regex.replace_all(&file_contents, "");

    // Fix empty sections, replace with "No significant changes"
    let empty_section_regex = Regex::new(r"(## .+)(\n\n\n)(?=## )").unwrap();
    let file_contents = empty_section_regex.replace_all(&file_contents, "$1\n\nNo significant changes.$2");

    // Remove issue links
    let empty_section_regex = Regex::new(r"\s*\[#\d*\]\(.*issues.*\)").unwrap();
    let file_contents = empty_section_regex.replace_all(&file_contents, "");

    // Insert preamble
    let file_contents = format!("{}{file_contents}", preamble_with_date().as_str());

    // Print the file contents
    println!("{}", file_contents);
}

pub fn preamble_with_date() -> String {
    format!(
        "---\n\
        title: Operator Changelog\n\
        date: 2023-08-15T00:00:00.000Z\n\
        lastmod: {}T00:00:00.000Z\n\
        draft: false\n\
        images: []\n\
        weight: 100\n\
        toc: true\n\
        tags:\n  - team\n  - enterprise\n\
        description: >-\n  The release changelog for the mirrord operator.\n\
        ---\n\n",
        Utc::now().date_naive()
    
    )
}
