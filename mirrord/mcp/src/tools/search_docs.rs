//! The `search_docs` tool: keyword search over the vendored docs and skills, so an agent finds the
//! page that answers its question without network access.
//!
//! Pages are ranked with [BM25](https://en.wikipedia.org/wiki/Okapi_BM25), with title matches
//! weighted up and words that start with a query word weighted down. The index is kept in sorted
//! maps and ties are broken by path, so a query ranks the same way every time, which a hash map's
//! per-process ordering wouldn't guarantee.

use std::{
    collections::{BTreeMap, BTreeSet},
    ops::Not,
    sync::LazyLock,
};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    corpus::{PAGES, front_matter},
    tools::explain_config_option::OPTION_PAGES,
};

/// Hits returned when the agent doesn't ask for a number.
const DEFAULT_LIMIT: usize = 5;
const MAX_LIMIT: usize = 20;

/// BM25's term frequency saturation and length normalization, at their usual values.
const K1: f64 = 1.2;
const B: f64 = 0.75;

/// How many body occurrences a term in the title counts as.
const TITLE_WEIGHT: u32 = 3;

/// How much a word that only starts with a query word counts, against an exact match, so that
/// `postgres` finds `postgresql` without outranking pages that say `postgres`.
const PREFIX_WEIGHT: f64 = 0.5;

/// The shortest query word that also matches longer words starting with it. Shorter ones, such as
/// `env`, would match too much.
const MIN_PREFIX_LENGTH: usize = 4;

/// Words too common to tell pages apart, left out of the index and of queries so that a question
/// such as "how do I run without a target" ranks by its keywords. Words that are also mirrord
/// terms, such as `up`, `all` and `off`, are kept.
const STOP_WORDS: &[&str] = &[
    "a", "about", "an", "and", "any", "are", "as", "at", "be", "but", "by", "can", "could", "do",
    "does", "for", "from", "get", "has", "have", "how", "i", "if", "is", "it", "its", "me", "my",
    "of", "or", "should", "so", "that", "the", "their", "then", "there", "these", "this", "to",
    "use", "using", "was", "we", "what", "when", "where", "which", "who", "why", "will", "with",
    "would", "you", "your",
];

/// The length a snippet is cut to, in characters.
const SNIPPET_LENGTH: usize = 200;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SearchDocsArgs {
    /// Keywords to search for, e.g. `steal http filter`.
    query: String,
    /// How many hits to return, from 1 to 20.
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

/// A searchable section of a page, or a config option, tokenized.
struct Document {
    path: &'static str,
    title: &'static str,
    resource_uri: &'static str,
    /// The section's heading, weighted like the title.
    heading: Option<&'static str>,
    /// What is searched, and the snippet is taken from.
    text: &'static str,
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

/// The sections of the markdown of the docs and skills, and every config option with a
/// description. A page is searched by section so that a long page ranks on the part that matches,
/// rather than having the match diluted by the rest, and its snippet comes from that part. Left out
/// are
/// pages that would crowd out the ones answering a query, which can still be read with `read_doc`:
/// - the files bundled with skills that aren't markdown, such as Helm values, which match nearly
///   any keyword;
/// - the skills' `README.md`s, short summaries of their `SKILL.md` for people browsing the repo.
static INDEX: LazyLock<Index> = LazyLock::new(|| {
    let searchable = |path: &str| {
        path.ends_with(".md") && (path.starts_with("skills/") && path.ends_with("/README.md")).not()
    };
    let pages = PAGES
        .iter()
        .filter(|(path, _)| searchable(path))
        .flat_map(|(path, page)| {
            sections(without_front_matter(page.body))
                .into_iter()
                .map(|(heading, text)| Document {
                    path,
                    title: &page.title,
                    resource_uri: &page.resource_uri,
                    heading,
                    text,
                    terms: BTreeMap::new(),
                    length: 0,
                })
        });
    let options = OPTION_PAGES.iter().map(|option| Document {
        path: &option.path,
        title: &option.title,
        resource_uri: &option.resource_uri,
        heading: None,
        text: &option.description,
        terms: BTreeMap::new(),
        length: 0,
    });
    let documents: Vec<Document> = pages
        .chain(options)
        .map(|mut document| {
            for term in tokens(document.text) {
                *document.terms.entry(term).or_default() += 1;
                document.length += 1;
            }
            for term in tokens(document.title).chain(document.heading.into_iter().flat_map(tokens))
            {
                *document.terms.entry(term).or_default() += TITLE_WEIGHT;
                document.length += TITLE_WEIGHT;
            }
            document
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

#[derive(Debug, Error)]
pub enum SearchDocsError {
    #[error("`limit` must be between 1 and {MAX_LIMIT}, got {0}")]
    InvalidLimit(usize),
}

pub fn search_docs(args: SearchDocsArgs) -> Result<SearchDocsOutput, SearchDocsError> {
    let limit = args.limit.unwrap_or(DEFAULT_LIMIT);
    if (1..=MAX_LIMIT).contains(&limit).not() {
        return Err(SearchDocsError::InvalidLimit(limit));
    }
    let query: BTreeSet<String> = tokens(&args.query).collect();
    let index = &*INDEX;
    let count = index.documents.len() as f64;

    // How rare a term is. A word that isn't in the index is as rare as can be.
    let idf = |term: &str| {
        let containing = index.document_frequency.get(term).copied().unwrap_or(0);
        let containing = f64::from(containing);
        (1.0 + (count - containing + 0.5) / (containing + 0.5)).ln()
    };

    // The index terms each query word matches, with how much each counts.
    let matched: Vec<(&str, Vec<(&str, f64)>)> = query
        .iter()
        .map(|word| {
            let terms = index
                .document_frequency
                .range(word.clone()..)
                .map(|(term, _)| term.as_str())
                .take_while(|term| term.starts_with(word.as_str()))
                .filter_map(|term| match term == word {
                    true => Some((term, 1.0)),
                    false => (word.len() >= MIN_PREFIX_LENGTH).then_some((term, PREFIX_WEIGHT)),
                })
                .collect();
            (word.as_str(), terms)
        })
        .collect();

    // A query word scores by its best match in the document, so a word that starts many others
    // (`target`: `targets`, `targetless`) doesn't add them all up, and a longer word counts as
    // no rarer than the query word, so a common one (`mirrord`) doesn't weigh in through rare
    // identifiers that start with it.
    let mut scored: Vec<(f64, &Document)> = index
        .documents
        .iter()
        .filter_map(|document| {
            let normalization = 1.0 - B + B * f64::from(document.length) / index.average_length;
            let score: f64 = matched
                .iter()
                .map(|(word, terms)| {
                    terms
                        .iter()
                        .filter_map(|(term, weight)| {
                            let frequency = f64::from(*document.terms.get(*term)?);
                            let idf = idf(term).min(idf(word));
                            Some(
                                weight * idf * frequency * (K1 + 1.0)
                                    / (frequency + K1 * normalization),
                            )
                        })
                        .fold(0.0, f64::max)
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

    // A page is hit once, through its best section. Some skill references repeat a docs page's
    // sections word for word, and a section with the same title and text as a better one adds
    // nothing but takes a slot.
    let mut pages = BTreeSet::new();
    let mut sections = BTreeSet::new();
    let hits = scored
        .into_iter()
        .map(|(_, document)| document)
        .filter(|document| {
            pages.insert(document.path) && sections.insert((document.title, document.text))
        })
        .take(limit)
        .map(|document| SearchHit {
            title: document.title.to_owned(),
            path: document.path.to_owned(),
            resource_uri: document.resource_uri.to_owned(),
            snippet: snippet(document.text, &query),
        })
        .collect();
    Ok(SearchDocsOutput { hits })
}

/// Lowercased words. Identifiers such as `http_filter` count both whole and by their parts, so an
/// exact identifier ranks its pages first while `http filter` still finds them.
fn tokens(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|character: char| (character.is_alphanumeric() || character == '_').not())
        .flat_map(|word| {
            let parts = word.split('_').filter(|_| word.contains('_'));
            std::iter::once(word).chain(parts)
        })
        .map(str::to_lowercase)
        .filter(|token| token.is_empty().not() && STOP_WORDS.contains(&token.as_str()).not())
}

/// Splits markdown at its headings, outside code blocks, into each section's heading and text
/// (heading line included). The text before the first heading has no heading, and is left out when
/// blank.
fn sections(markdown: &str) -> Vec<(Option<&str>, &str)> {
    let mut sections = Vec::new();
    let mut heading = None;
    let mut start = 0;
    let mut in_code = false;
    let mut offset = 0;
    for line in markdown.split_inclusive('\n') {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            in_code = in_code.not();
        } else if in_code.not() && trimmed.starts_with('#') {
            if let Some(text) = markdown.get(start..offset) {
                sections.push((heading, text));
            }
            heading = Some(trimmed.trim_start_matches('#').trim());
            start = offset;
        }
        offset += line.len();
    }
    if let Some(text) = markdown.get(start..) {
        sections.push((heading, text));
    }
    sections.retain(|(heading, text)| heading.is_some() || text.trim().is_empty().not());
    sections
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
        .unwrap()
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

    #[test]
    fn skips_repeated_sections() {
        let hits = search("turbo task");
        let troubleshooting = hits.iter().filter(|path| {
            path.ends_with("troubleshooting.md") || path.ends_with("common-issues.md")
        });
        assert_eq!(troubleshooting.count(), 1, "{hits:?}");
    }

    #[test]
    fn rejects_out_of_range_limits() {
        for limit in [0, MAX_LIMIT + 1] {
            let args = SearchDocsArgs {
                query: "targetless".to_owned(),
                limit: Some(limit),
            };
            assert!(search_docs(args).is_err(), "{limit}");
        }
    }

    #[test]
    fn splits_sections_outside_code() {
        let markdown = "intro\n# One\n```sh\n# a comment\n```\n## Two\ntext\n";
        assert_eq!(
            sections(markdown),
            [
                (None, "intro\n"),
                (Some("One"), "# One\n```sh\n# a comment\n```\n"),
                (Some("Two"), "## Two\ntext\n"),
            ]
        );
    }
}
