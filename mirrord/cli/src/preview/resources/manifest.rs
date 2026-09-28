//! Step 1 of the `--resource` pipeline: reading the user's YAML manifests into objects.
//!
//! Runs entirely on this machine, before the cluster is contacted, so a typo in a file fails
//! the command without touching anything.

use std::{
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
};

use serde_json::Value;

use super::{ResourcesError, SuppliedObject};

/// File extensions read from a directory, and accepted for a file passed directly.
const MANIFEST_EXTENSIONS: [&str; 2] = ["yaml", "yml"];

/// Reads every object from `paths`, in order. A directory contributes its `*.yaml` / `*.yml`
/// files (not recursively, like `kubectl apply -f <dir>`), sorted by name so the order is the
/// same on every machine.
pub(crate) fn load(paths: &[PathBuf]) -> Result<Vec<SuppliedObject>, ResourcesError> {
    let mut objects = Vec::new();

    for path in paths {
        for file in manifest_files(path)? {
            let text = fs::read_to_string(&file).map_err(|source| ResourcesError::Read {
                path: file.clone(),
                source,
            })?;
            objects.extend(parse(&file, &text)?);
        }
    }

    Ok(objects)
}

/// The manifest files `path` stands for.
fn manifest_files(path: &Path) -> Result<Vec<PathBuf>, ResourcesError> {
    let metadata = fs::metadata(path).map_err(|source| match source.kind() {
        std::io::ErrorKind::NotFound => ResourcesError::PathMissing(path.to_path_buf()),
        _ => ResourcesError::Read {
            path: path.to_path_buf(),
            source,
        },
    })?;

    if !metadata.is_dir() {
        return if is_manifest(path) {
            Ok(vec![path.to_path_buf()])
        } else {
            Err(ResourcesError::NotYaml(path.to_path_buf()))
        };
    }

    let entries = fs::read_dir(path).map_err(|source| ResourcesError::Read {
        path: path.to_path_buf(),
        source,
    })?;

    let mut files = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| ResourcesError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        let file = entry.path();
        if file.is_file() && is_manifest(&file) {
            files.push(file);
        }
    }

    if files.is_empty() {
        return Err(ResourcesError::NoManifests(path.to_path_buf()));
    }

    files.sort();
    Ok(files)
}

fn is_manifest(path: &Path) -> bool {
    path.extension()
        .and_then(OsStr::to_str)
        .is_some_and(|extension| MANIFEST_EXTENSIONS.contains(&extension))
}

/// Parses every YAML document in `text`. Empty documents (a trailing `---`) are skipped, and a
/// `kind: List` contributes its items, matching what `kubectl apply` accepts.
pub(crate) fn parse(path: &Path, text: &str) -> Result<Vec<SuppliedObject>, ResourcesError> {
    let options = serde_saphyr::options! { with_snippet: false };
    let documents: Vec<Value> = serde_saphyr::from_multiple_with_options(text, options)
        .map_err(|error| parse_error(path, &error))?;

    let mut objects = Vec::new();
    for (index, document) in documents.into_iter().enumerate() {
        match document {
            Value::Null => {}
            Value::Object(ref object)
                if object.get("kind").and_then(Value::as_str) == Some("List") =>
            {
                let items = object
                    .get("items")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                for item in items {
                    objects.push(SuppliedObject::new(path, index, item)?);
                }
            }
            document => objects.push(SuppliedObject::new(path, index, document)?),
        }
    }

    Ok(objects)
}

/// The parser's message with its location printed once, in front, the way the error is
/// documented, instead of in the parser's own suffix.
fn parse_error(path: &Path, error: &serde_saphyr::Error) -> ResourcesError {
    let rendered = error.to_string();
    let location = error
        .location()
        .map(|location| (location.line(), location.column()))
        .filter(|&(line, column)| line > 0 && column > 0);

    let message = match location {
        Some((line, column)) => strip_location(&rendered, line, column),
        None => rendered,
    };

    ResourcesError::Parse {
        path: path.to_path_buf(),
        location,
        message,
    }
}

/// Removes the parser's `at line L, column C` style suffix from its message.
fn strip_location(message: &str, line: u64, column: u64) -> String {
    let first_line = message.lines().next().unwrap_or_default();
    for suffix in [
        format!(" at line {line}, column {column}"),
        format!(" at line {line} column {column}"),
    ] {
        if let Some(stripped) = first_line.strip_suffix(&suffix) {
            return stripped.to_owned();
        }
    }
    first_line.to_owned()
}

#[cfg(test)]
#[allow(clippy::indexing_slicing)] // Tests read JSON fixtures with `value[key]`; a panic just fails the test.
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn multi_document_file_yields_every_object_and_skips_empty_documents() {
        let text = "\
apiVersion: v1
kind: ConfigMap
metadata:
  name: one
---
---
apiVersion: v1
kind: Secret
metadata:
  name: two
  namespace: staging
";
        let objects = parse(Path::new("./k8s/all.yaml"), text).unwrap();

        let names: Vec<_> = objects
            .iter()
            .map(|object| {
                (
                    object.kind.as_str(),
                    object.name.as_str(),
                    object.namespace.as_deref(),
                )
            })
            .collect();
        assert_eq!(
            names,
            [
                ("ConfigMap", "one", None),
                ("Secret", "two", Some("staging"))
            ]
        );
        assert!(
            objects
                .iter()
                .all(|object| object.source == Path::new("./k8s/all.yaml"))
        );
    }

    #[test]
    fn list_documents_are_flattened() {
        let text = "\
apiVersion: v1
kind: List
items:
- kind: ConfigMap
  metadata: {name: a}
- kind: ConfigMap
  metadata: {name: b}
";
        let names: Vec<_> = parse(Path::new("list.yaml"), text)
            .unwrap()
            .into_iter()
            .map(|object| object.name)
            .collect();
        assert_eq!(names, ["a", "b"]);
    }

    /// The documented message: `Failed to parse <file>: line L, column C: <parser message>`,
    /// with the location printed once.
    #[test]
    fn invalid_yaml_reports_file_line_and_column_once() {
        let text = "\
apiVersion: v1
kind: ConfigMap
metadata:
  name: app
data:
  a: b
  c: d: e
";
        let error = parse(Path::new("./k8s/configmap.yaml"), text).unwrap_err();

        let rendered = error.to_string();
        assert!(
            rendered.starts_with(
                "Failed to parse ./k8s/configmap.yaml: line 7, column 7: mapping values are not \
                 allowed in this context"
            ),
            "{rendered}"
        );
        assert!(!rendered.contains("at line"), "{rendered}");
        assert!(rendered.ends_with("Nothing was created."), "{rendered}");
    }

    #[test]
    fn document_without_kind_or_name_is_rejected_with_its_position() {
        let error = parse(
            Path::new("x.yaml"),
            "kind: ConfigMap\n---\nmetadata: {name: a}\n",
        )
        .unwrap_err();
        assert!(
            matches!(&error, ResourcesError::NotAnObject { index: 0, .. }),
            "{error}"
        );
    }

    #[test]
    fn missing_path_is_reported() {
        let error = load(&[PathBuf::from("./definitely/not/here/")]).unwrap_err();
        assert_eq!(
            error.to_string(),
            "Path ./definitely/not/here/ does not exist."
        );
    }

    #[test]
    fn directory_without_manifests_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("notes.txt"), "hello").unwrap();

        let error = load(&[dir.path().to_path_buf()]).unwrap_err();
        assert_eq!(
            error.to_string(),
            format!(
                "No Kubernetes manifests found in {} (looked for *.yaml, *.yml).",
                dir.path().display()
            )
        );
    }

    #[test]
    fn non_yaml_file_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let notes = dir.path().join("notes.txt");
        fs::write(&notes, "hello").unwrap();

        let error = load(std::slice::from_ref(&notes)).unwrap_err();
        assert_eq!(
            error.to_string(),
            format!("{} is not a YAML file.", notes.display())
        );
    }

    #[test]
    fn directory_reads_yaml_and_yml_sorted_and_ignores_other_files() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("b.yml"),
            "kind: ConfigMap\nmetadata: {name: second}\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("a.yaml"),
            "kind: ConfigMap\nmetadata: {name: first}\n",
        )
        .unwrap();
        fs::write(dir.path().join("README.md"), "# not a manifest").unwrap();
        fs::create_dir(dir.path().join("nested")).unwrap();
        fs::write(
            dir.path().join("nested/c.yaml"),
            "kind: ConfigMap\nmetadata: {name: nested}\n",
        )
        .unwrap();

        let names: Vec<_> = load(&[dir.path().to_path_buf()])
            .unwrap()
            .into_iter()
            .map(|object| object.name)
            .collect();
        assert_eq!(names, ["first", "second"]);
    }
}
