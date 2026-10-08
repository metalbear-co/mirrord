//! The `search_docs` tool: keyword search over the vendored docs and skills, so an agent finds the
//! page that answers its question without network access.
//!
//! Pages are ranked with [BM25](https://en.wikipedia.org/wiki/Okapi_BM25), with title matches
//! weighted up. The index is kept in sorted maps and ties are broken by path, so a query ranks the
//! same way every time, which a hash map's per-process ordering wouldn't guarantee.

use std::{
    collections::{BTreeMap, BTreeSet},
    ops::Not,
    sync::LazyLock,
};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::corpus::{PAGES, Page, front_matter};

/// Hits returned when the agent doesn't ask for a number.
const DEFAULT_LIMIT: usize = 5;
const MAX_LIMIT: usize = 20;

/// BM25's term frequency saturation and length normalization, at their usual values.
const K1: f64 = 1.2;
const B: f64 = 0.75;

/// How many body occurrences a term in the title counts as.
const TITLE_WEIGHT: u32 = 3;

/// The length a snippet is cut to, in characters.
const SNIPPET_LENGTH: usize = 200;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SearchDocsArgs {
    /// Keywords to search for, e.g. `steal http filter`.
    query: String,
    /// How many hits to return, at most 20.
    #[serde(default)]
    limit: Option<usize>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct SearchDocsOutput {
    /// The matching pages, best first. Empty when nothing matches.
    hits: Vec<SearchHit>,
}

#[derive(Debug, Serialize, JsonSchema)]
struct SearchHit {
    title: String,
    /// The page's path, which `read_doc` takes.
    path: String,
    /// The MCP resource serving the page.
    resource_uri: String,
    /// The line of the page that best matches the query.
    snippet: String,
}

/// A searchable page, tokenized.
struct Document {
    path: &'static str,
    page: &'static Page,
    /// How often each term occurs, title occurrences weighted by [`TITLE_WEIGHT`].
    terms: BTreeMap<String, u32>,
    length: u32,
}

struct Index {
    documents: Vec<Document>,
    /// How many documents each term occurs in.
    document_frequency: BTreeMap<String, u32>,
    average_length: f64,
}

/// The markdown of the docs and skills, except for pages that would crowd out the ones answering a
/// query, which can still be read with `read_doc`:
/// - the skills' JSON schemas and Helm values, which match nearly any keyword;
/// - `SUMMARY.md`, which is only the docs' table of contents;
/// - the skills' `README.md`s, short summaries of their `SKILL.md` for people browsing the repo.
static INDEX: LazyLock<Index> = LazyLock::new(|| {
    let searchable = |path: &str| {
        path.ends_with(".md")
            && path != "docs/SUMMARY.md"
            && (path.starts_with("skills/") && path.ends_with("/README.md")).not()
    };
    let documents: Vec<Document> = PAGES
        .iter()
        .filter(|(path, _)| searchable(path))
        .map(|(path, page)| {
            let mut terms = BTreeMap::new();
            let mut length = 0;
            for term in tokens(without_front_matter(page.body)) {
                *terms.entry(term).or_default() += 1;
                length += 1;
            }
            for term in tokens(&page.title) {
                *terms.entry(term).or_default() += TITLE_WEIGHT;
                length += TITLE_WEIGHT;
            }
            Document {
                path,
                page,
                terms,
                length,
            }
        })
        .collect();

    let mut document_frequency = BTreeMap::new();
    for document in &documents {
        for term in document.terms.keys() {
            *document_frequency.entry(term.clone()).or_default() += 1;
        }
    }
    let total_length: f64 = documents
        .iter()
        .map(|document| f64::from(document.length))
        .sum();
    Index {
        average_length: total_length / documents.len().max(1) as f64,
        documents,
        document_frequency,
    }
});

pub fn search_docs(args: SearchDocsArgs) -> SearchDocsOutput {
    let limit = args.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let query: BTreeSet<String> = tokens(&args.query).collect();
    let index = &*INDEX;
    let count = index.documents.len() as f64;

    let mut scored: Vec<(f64, &Document)> = index
        .documents
        .iter()
        .filter_map(|document| {
            let score: f64 = query
                .iter()
                .filter_map(|term| {
                    let frequency = f64::from(*document.terms.get(term)?);
                    let containing = f64::from(*index.document_frequency.get(term)?);
                    let idf = (1.0 + (count - containing + 0.5) / (containing + 0.5)).ln();
                    let normalization =
                        1.0 - B + B * f64::from(document.length) / index.average_length;
                    Some(idf * frequency * (K1 + 1.0) / (frequency + K1 * normalization))
                })
                .sum();
            (score > 0.0).then_some((score, document))
        })
        .collect();
    scored.sort_by(|(score_a, document_a), (score_b, document_b)| {
        score_b
            .total_cmp(score_a)
            .then(document_a.path.cmp(document_b.path))
    });

    let hits = scored
        .into_iter()
        .take(limit)
        .map(|(_, document)| SearchHit {
            title: document.page.title.clone(),
            path: document.path.to_owned(),
            resource_uri: document.page.resource_uri.clone(),
            snippet: snippet(without_front_matter(document.page.body), &query),
        })
        .collect();
    SearchDocsOutput { hits }
}

/// Lowercased words. Identifiers such as `http_filter` count both whole and by their parts, so an
/// exact identifier ranks its pages first while `http filter` still finds them.
fn tokens(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|character: char| (character.is_alphanumeric() || character == '_').not())
        .flat_map(|word| {
            let parts = word.split('_').filter(|_| word.contains('_'));
            std::iter::once(word).chain(parts)
        })
        .filter(|token| token.is_empty().not())
        .map(str::to_lowercase)
}

fn without_front_matter(body: &str) -> &str {
    front_matter(body)
        .and_then(|front_matter| body.get("---\n".len() + front_matter.len() + "\n---\n".len()..))
        .unwrap_or(body)
}

/// The first of the lines with the most distinct query terms, cut to [`SNIPPET_LENGTH`] around its
/// first match.
fn snippet(body: &str, query: &BTreeSet<String>) -> String {
    let matches = |line: &str| {
        tokens(line)
            .filter(|token| query.contains(token))
            .collect::<BTreeSet<_>>()
            .len()
    };
    let Some(line) = body
        .lines()
        .map(str::trim)
        .filter(|line| line.is_empty().not())
        .rev()
        .max_by_key(|line| matches(line))
    else {
        return String::new();
    };

    let lowercase = line.to_lowercase();
    let first_match = query
        .iter()
        .filter_map(|term| lowercase.find(term.as_str()))
        .min()
        .unwrap_or(0);
    let start = lowercase
        .get(..first_match)
        .map_or(0, |before| before.chars().count())
        .saturating_sub(SNIPPET_LENGTH / 4);
    let snippet: String = line.chars().skip(start).take(SNIPPET_LENGTH).collect();
    let mut snippet = if start > 0 {
        format!("…{snippet}")
    } else {
        snippet
    };
    if start + SNIPPET_LENGTH < line.chars().count() {
        snippet.push('…');
    }
    snippet
}

#[cfg(test)]
mod tests {
    use super::*;

    fn search(query: &str) -> Vec<String> {
        search_docs(SearchDocsArgs {
            query: query.to_owned(),
            limit: None,
        })
        .hits
        .into_iter()
        .map(|hit| hit.path)
        .collect()
    }

    #[test]
    fn ranks_the_matching_page_first() {
        assert_eq!(
            search("running without a target")
                .first()
                .map(String::as_str),
            Some("docs/using-mirrord/targetless.md")
        );
    }

    #[test]
    fn ranks_the_same_way_every_time() {
        let first = search("steal http filter");
        assert_eq!(first.len(), DEFAULT_LIMIT);
        assert_eq!(first, search("steal http filter"));
        assert_eq!(first, search("STEAL HTTP Filter"));
    }
}
