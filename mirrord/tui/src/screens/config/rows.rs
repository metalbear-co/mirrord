//! The config options as the rows of a tree, folded and filtered the way the user asked.

use serde_json::Value;

use super::document::Document;
use crate::{
    config_schema::{Field, Shape},
    kit::tree::{Fold, TreeId, TreeNav, TreeRow},
};

/// The keys from the top of the config down to an option, such as `feature.network.incoming`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct OptionPath(pub Vec<String>);

impl TreeId for OptionPath {
    fn parent(&self) -> Option<Self> {
        let (_, parent) = self.0.split_last()?;
        (!parent.is_empty()).then(|| Self(parent.to_vec()))
    }
}

impl OptionPath {
    pub fn dotted(&self) -> String {
        self.0.join(".")
    }
}

/// One visible option.
pub struct Row {
    pub path: OptionPath,
    pub field: &'static Field,
    fold: Fold,
}

impl TreeRow for Row {
    type Id = OptionPath;

    fn id(&self) -> OptionPath {
        self.path.clone()
    }

    fn fold(&self) -> Fold {
        self.fold
    }
}

impl Row {
    pub fn depth(&self) -> usize {
        self.path.0.len().saturating_sub(1)
    }
}

/// Which options to list.
#[derive(Default)]
pub struct Filter {
    /// Lower-cased search phrase, matched against each option's path. Empty matches everything.
    pub phrase: String,
    /// Also match the phrase against each option's docs. [`flatten`] turns this on by itself
    /// when no path matches, since docs mention other options' names often enough to bury
    /// the option being looked for.
    pub docs: bool,
    /// List only options that have a value of their own.
    pub set_only: bool,
}

impl Filter {
    fn active(&self) -> bool {
        !self.phrase.is_empty() || self.set_only
    }

    fn matches(&self, path: &OptionPath, field: &Field, value: Option<&Value>) -> bool {
        if self.set_only && value.is_none() {
            return false;
        }
        self.phrase.is_empty()
            || path.dotted().to_lowercase().contains(&self.phrase)
            || (self.docs
                && field
                    .doc
                    .as_deref()
                    .is_some_and(|doc| doc.to_lowercase().contains(&self.phrase)))
    }
}

/// The options an option opens into: a section's options, or those of the settings variant of
/// an option that takes several forms. Such an option opens while it is unset or holds settings
/// (`incoming: { ... }`), and stays closed while it holds another form (`incoming: "steal"`).
pub fn children<'a>(field: &'a Field, value: Option<&Value>) -> Option<&'a [Field]> {
    match (&field.shape, value) {
        (Shape::Group(fields), _) => Some(fields),
        (shape @ Shape::Variants(_), Some(value)) => match shape.active_variant(value)?.shape {
            Shape::Group(ref fields) => Some(fields),
            _ => None,
        },
        (shape, None) => settings(shape),
        _ => None,
    }
}

/// The options of the settings variant of `shape`, if it has one.
fn settings(shape: &Shape) -> Option<&[Field]> {
    let Shape::Variants(variants) = shape else {
        return None;
    };
    variants.iter().find_map(|variant| match &variant.shape {
        Shape::Group(fields) => Some(fields.as_slice()),
        _ => None,
    })
}

/// The rows to draw, in tree order.
///
/// While a filter is on, every group with a matching option inside is shown open, so a search
/// never hides its own results behind a fold.
pub fn flatten(
    fields: &'static [Field],
    document: &Document,
    nav: &TreeNav<OptionPath>,
    filter: &Filter,
) -> Vec<Row> {
    let mut rows = Vec::new();
    walk(fields, &[], document, nav, filter, &mut rows);

    if rows.is_empty() && !filter.phrase.is_empty() && !filter.docs {
        let filter = Filter {
            phrase: filter.phrase.clone(),
            docs: true,
            set_only: filter.set_only,
        };
        walk(fields, &[], document, nav, &filter, &mut rows);
    }
    rows
}

/// Appends the rows of `fields` under `parent`. Returns whether it appended any.
fn walk(
    fields: &'static [Field],
    parent: &[String],
    document: &Document,
    nav: &TreeNav<OptionPath>,
    filter: &Filter,
    rows: &mut Vec<Row>,
) -> bool {
    let before = rows.len();

    for field in fields {
        let mut keys = parent.to_vec();
        keys.push(field.key.clone());
        let path = OptionPath(keys);
        let value = document.get(&path.0);
        let children = children(field, value).filter(|children| !children.is_empty());

        let Some(children) = children else {
            if !filter.active() || filter.matches(&path, field, value) {
                rows.push(Row {
                    path,
                    field,
                    fold: Fold::Leaf,
                });
            }
            continue;
        };

        if filter.active() {
            let at = rows.len();
            let any_child = walk(children, &path.0, document, nav, filter, rows);
            if any_child || filter.matches(&path, field, value) {
                rows.insert(
                    at,
                    Row {
                        path,
                        field,
                        fold: Fold::Expanded,
                    },
                );
            }
            continue;
        }

        let collapsed = nav.is_collapsed(&path);
        let keys = path.0.clone();
        rows.push(Row {
            path,
            field,
            fold: if collapsed {
                Fold::Collapsed
            } else {
                Fold::Expanded
            },
        });
        if !collapsed {
            walk(children, &keys, document, nav, filter, rows);
        }
    }

    rows.len() > before
}

/// Every path in `fields` that can open into options, for a tree that starts with all of them
/// folded.
pub fn section_paths(fields: &[Field], parent: &[String], out: &mut Vec<OptionPath>) {
    for field in fields {
        let children = match &field.shape {
            Shape::Group(children) => Some(children.as_slice()),
            shape => settings(shape),
        };
        if let Some(children) = children {
            let mut keys = parent.to_vec();
            keys.push(field.key.clone());
            section_paths(children, &keys, out);
            out.push(OptionPath(keys));
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{Filter, OptionPath, flatten, section_paths};
    use crate::{
        config_schema::FIELDS,
        kit::tree::{TreeNav, TreeRow},
        screens::config::document::Document,
    };

    fn empty() -> Document {
        Document::load(&std::env::temp_dir().join("mirrord-tui-no-such-dir"))
    }

    fn folded() -> TreeNav<OptionPath> {
        let mut nav = TreeNav::default();
        let mut sections = Vec::new();
        section_paths(&FIELDS, &[], &mut sections);
        for section in sections {
            nav.collapse(section);
        }
        nav
    }

    #[test]
    fn starts_with_only_top_level_options() {
        let rows = flatten(&FIELDS, &empty(), &folded(), &Filter::default());
        assert_eq!(rows.len(), FIELDS.len());
        assert!(rows.iter().all(|row| row.depth() == 0));
    }

    #[test]
    fn search_opens_the_sections_holding_a_match() {
        let filter = Filter {
            phrase: "http_filter".to_owned(),
            ..Filter::default()
        };
        let mut document = empty();
        let incoming = [
            "feature".to_owned(),
            "network".to_owned(),
            "incoming".to_owned(),
        ];
        let rows = flatten(&FIELDS, &document, &folded(), &filter);
        assert!(
            rows.iter()
                .any(|row| row.id().dotted() == "feature.network.incoming.http_filter")
        );

        // Holding a mode string, `incoming` has no settings to open into.
        document.set(&incoming, json!("steal"));
        assert!(
            !flatten(&FIELDS, &document, &folded(), &filter)
                .iter()
                .any(|row| row.id().dotted() == "feature.network.incoming.http_filter")
        );

        document.set(
            &[
                "feature".to_owned(),
                "network".to_owned(),
                "incoming".to_owned(),
            ],
            json!({ "mode": "steal" }),
        );
        let rows = flatten(&FIELDS, &document, &folded(), &filter);
        let paths: Vec<String> = rows.iter().map(|row| row.id().dotted()).collect();
        assert!(
            paths
                .iter()
                .any(|path| path == "feature.network.incoming.http_filter"),
            "{paths:#?}"
        );
        assert!(paths.contains(&"feature".to_owned()));
        // Docs mentioning `http_filter` do not pull their options in while paths match.
        assert!(!paths.contains(&"key".to_owned()), "{paths:#?}");

        // With no path to match, the docs are searched instead.
        let filter = Filter {
            phrase: "w3c `baggage`".to_owned(),
            ..Filter::default()
        };
        let rows = flatten(&FIELDS, &document, &folded(), &filter);
        assert!(rows.iter().any(|row| row.id().dotted() == "key"));
    }

    #[test]
    fn set_only_lists_what_the_file_holds() {
        let mut document = empty();
        document.set(&["feature".to_owned(), "fs".to_owned()], json!("read"));
        let filter = Filter {
            set_only: true,
            ..Filter::default()
        };
        let rows = flatten(&FIELDS, &document, &folded(), &filter);
        let paths: Vec<String> = rows.iter().map(|row| row.id().dotted()).collect();
        assert_eq!(paths, ["feature", "feature.fs"]);
    }
}
