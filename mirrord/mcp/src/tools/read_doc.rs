//! The `read_doc` tool: serves one page of the vendored docs and skills in full, by the path
//! `search_docs` returns, or lists every page.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::corpus::PAGES;

/// How many paths an unknown path suggests.
const SUGGESTIONS: usize = 5;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ReadDocArgs {
    /// The page's path, as `search_docs` returns it, e.g. `docs/using-mirrord/targetless.md`.
    /// Leave out to list every page.
    #[serde(default)]
    path: Option<String>,
}

#[derive(Debug, Default, Serialize, JsonSchema)]
pub struct ReadDocOutput {
    /// Every page, when no `path` was given.
    #[serde(skip_serializing_if = "Option::is_none")]
    pages: Option<Vec<PageSummary>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    /// Where the page is published, for linking the user to it.
    #[serde(skip_serializing_if = "Option::is_none")]
    source_url: Option<String>,
    /// The page's full markdown.
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
struct PageSummary {
    path: String,
    title: String,
}

#[derive(Debug, Error)]
pub enum ReadDocError {
    #[error("unknown page `{path}`, the closest pages are: {closest}")]
    UnknownPage { path: String, closest: String },
}

pub fn read_doc(args: ReadDocArgs) -> Result<ReadDocOutput, ReadDocError> {
    let Some(path) = args.path else {
        let pages = PAGES
            .iter()
            .map(|(path, page)| PageSummary {
                path: (*path).to_owned(),
                title: page.title.clone(),
            })
            .collect();
        return Ok(ReadDocOutput {
            pages: Some(pages),
            ..Default::default()
        });
    };

    let Some(page) = PAGES.get(path.as_str()) else {
        return Err(ReadDocError::UnknownPage {
            closest: closest(&path),
            path,
        });
    };
    Ok(ReadDocOutput {
        title: Some(page.title.clone()),
        source_url: Some(page.source_url.clone()),
        content: Some(page.body.to_owned()),
        ..Default::default()
    })
}

/// The known paths closest to `asked`: those ending with it first (`targetless.md` for
/// `docs/using-mirrord/targetless.md`), then by edit distance, then by path.
fn closest(asked: &str) -> String {
    let mut ranked: Vec<(bool, f64, &str)> = PAGES
        .keys()
        .map(|path| {
            (
                path.ends_with(asked),
                strsim::normalized_levenshtein(asked, path),
                *path,
            )
        })
        .collect();
    ranked.sort_by(
        |(suffix_a, similarity_a, path_a), (suffix_b, similarity_b, path_b)| {
            suffix_b
                .cmp(suffix_a)
                .then(similarity_b.total_cmp(similarity_a))
                .then(path_a.cmp(path_b))
        },
    );
    ranked
        .into_iter()
        .take(SUGGESTIONS)
        .map(|(_, _, path)| format!("`{path}`"))
        .collect::<Vec<_>>()
        .join(", ")
}
