//! The `read_doc` tool: serves one page of the vendored docs and skills in full, by the path
//! `search_docs` returns, or lists every page.

use schemars::JsonSchema;
use serde::Deserialize;
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

#[derive(Debug, Error)]
pub enum ReadDocError {
    #[error("unknown page `{path}`, the closest pages are: {closest}")]
    UnknownPage { path: String, closest: String },
}

/// Answers in markdown: a page goes out as text, once, where structured output would also carry it
/// serialized as text, escaped.
pub fn read_doc(args: ReadDocArgs) -> Result<String, ReadDocError> {
    let Some(path) = args.path else {
        return Ok(PAGES
            .iter()
            .map(|(path, page)| format!("- `{path}`: {}\n", page.title))
            .collect());
    };

    let Some(page) = PAGES.get(path.as_str()) else {
        return Err(ReadDocError::UnknownPage {
            closest: closest(&path),
            path,
        });
    };
    Ok(format!(
        "Title: {}\nSource: {}\n\n{}",
        page.title, page.source_url, page.body
    ))
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
