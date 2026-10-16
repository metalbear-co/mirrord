//! The config file the Config screen edits: `mirrord.json`, `mirrord.yaml`, `mirrord.yml` or
//! `mirrord.toml` under `.mirrord/` in the directory the TUI was started from - the formats
//! `mirrord exec -f` reads. A new file is created as JSON, the one the IDE extensions pick up on
//! their own.
//!
//! Only options the user set are kept in it. An option that is left out falls back to mirrord's
//! default, which is the point of leaving it out, so the document never fills itself with
//! defaults.

use std::{
    io,
    path::{Path, PathBuf},
};

use mirrord_config::{LayerConfig, LayerFileConfig, config::ConfigContext};
use serde_json::{Map, Value};

/// The directory the file lives in, relative to the directory the TUI was started from.
const DIRECTORY: &str = ".mirrord";

/// The file names looked for, first match wins.
const FILE_NAMES: &[&str] = &[
    "mirrord.json",
    "mirrord.yaml",
    "mirrord.yml",
    "mirrord.toml",
];

/// How the file is written, decided by its extension the same way `mirrord exec -f` decides it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Format {
    Json,
    Yaml,
    Toml,
}

impl Format {
    fn of(path: &Path) -> Self {
        match path.extension().and_then(|extension| extension.to_str()) {
            Some("yaml" | "yml") => Self::Yaml,
            Some("toml") => Self::Toml,
            _ => Self::Json,
        }
    }

    fn parse(self, text: &str) -> Result<Value, String> {
        match self {
            Self::Json => serde_json::from_str(text).map_err(|error| error.to_string()),
            Self::Yaml => serde_saphyr::from_str(text).map_err(|error| error.to_string()),
            Self::Toml => toml::from_str(text).map_err(|error| error.to_string()),
        }
    }

    fn render(self, root: &Map<String, Value>) -> Result<String, String> {
        let mut text = match self {
            Self::Json => serde_json::to_string_pretty(root).map_err(|error| error.to_string())?,
            Self::Yaml => serde_saphyr::to_string(root).map_err(|error| error.to_string())?,
            Self::Toml => toml::to_string_pretty(root).map_err(|error| error.to_string())?,
        };
        if !text.ends_with('\n') {
            text.push('\n');
        }
        Ok(text)
    }

    /// Whether `text` has comments, which a rewrite of the file cannot keep. JSON has none.
    fn has_comments(self, text: &str) -> bool {
        self != Self::Json && text.lines().any(|line| line.trim_start().starts_with('#'))
    }
}

/// Every mirrord config file in `dir`'s `.mirrord` folder, whatever it is named, with what it
/// holds: teams often keep several (`mirrord.json`, `steal.json`, `db.yaml`) for different jobs.
/// Files that cannot be read as a map of options are left out.
pub fn config_files(dir: &Path) -> Vec<(PathBuf, Map<String, Value>)> {
    let Ok(entries) = std::fs::read_dir(dir.join(DIRECTORY)) else {
        return Vec::new();
    };
    let mut files: Vec<(PathBuf, Map<String, Value>)> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            matches!(
                path.extension().and_then(|extension| extension.to_str()),
                Some("json" | "yaml" | "yml" | "toml")
            )
        })
        .filter_map(|path| {
            let text = std::fs::read_to_string(&path).ok()?;
            let Value::Object(mut root) = Format::of(&path).parse(&text).ok()? else {
                return None;
            };
            drop_nulls(&mut root);
            Some((path, root))
        })
        .collect();
    files.sort_by(|(left, _), (right, _)| left.cmp(right));
    files
}

pub struct Document {
    path: PathBuf,
    format: Format,
    /// The file's top-level object.
    root: Map<String, Value>,
    /// Set when the file exists but could not be read - it may use templates mirrord renders
    /// before parsing. Saving is refused then, so the file is never replaced by an empty one.
    unreadable: Option<String>,
    /// Set when the file has comments, which saving drops.
    has_comments: bool,
    /// Whether there are edits the file does not have yet.
    dirty: bool,
}

impl Document {
    /// Reads the config file under `dir`, or starts an empty JSON document when there is none.
    pub fn load(dir: &Path) -> Self {
        let directory = dir.join(DIRECTORY);
        let path = FILE_NAMES
            .iter()
            .map(|name| directory.join(name))
            .find(|path| path.exists())
            .unwrap_or_else(|| directory.join("mirrord.json"));
        let format = Format::of(&path);

        let mut document = Self {
            path,
            format,
            root: Map::new(),
            unreadable: None,
            has_comments: false,
            dirty: false,
        };

        match std::fs::read_to_string(&document.path) {
            Ok(text) => {
                document.has_comments = format.has_comments(&text);
                match format.parse(&text) {
                    Ok(Value::Object(mut root)) => {
                        drop_nulls(&mut root);
                        document.root = root;
                    }
                    // An empty YAML file reads as null: a file with nothing set.
                    Ok(Value::Null) => {}
                    Ok(_) => {
                        document.unreadable = Some("it does not hold a map of options".to_owned())
                    }
                    Err(error) => document.unreadable = Some(error),
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => document.unreadable = Some(error.to_string()),
        }

        document
    }

    /// The file's path as the user would type it, relative to the directory the TUI started in.
    pub fn display_path(&self) -> String {
        let name = self
            .path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        format!("{DIRECTORY}/{name}")
    }

    /// Whether saving will drop comments the file has.
    pub fn has_comments(&self) -> bool {
        self.has_comments
    }

    pub fn unreadable(&self) -> Option<&str> {
        self.unreadable.as_deref()
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// The value set at `path`, if any.
    pub fn get(&self, path: &[String]) -> Option<&Value> {
        let (last, parents) = path.split_last()?;
        let mut object = &self.root;
        for key in parents {
            object = object.get(key)?.as_object()?;
        }
        object.get(last)
    }

    /// Sets the value at `path`, creating the sections above it as needed. A section that held
    /// a non-object value (`"incoming": "steal"` when setting `incoming.mode`) is replaced by one.
    pub fn set(&mut self, path: &[String], value: Value) {
        let Some((last, parents)) = path.split_last() else {
            return;
        };
        let mut object = &mut self.root;
        for key in parents {
            let entry = object
                .entry(key.clone())
                .or_insert_with(|| Value::Object(Map::new()));
            if !entry.is_object() {
                *entry = Value::Object(Map::new());
            }
            let Value::Object(next) = entry else {
                return;
            };
            object = next;
        }
        object.insert(last.clone(), value);
        self.dirty = true;
    }

    /// Removes the value at `path`, and any section left empty by that, so the option goes back
    /// to mirrord's default.
    pub fn unset(&mut self, path: &[String]) {
        if remove(&mut self.root, path) {
            self.dirty = true;
        }
    }

    /// How many options have a value of their own, counting each leaf value once.
    pub fn set_count(&self) -> usize {
        fn count(value: &Value) -> usize {
            match value {
                Value::Object(object) if !object.is_empty() => object.values().map(count).sum(),
                _ => 1,
            }
        }
        self.root.values().map(count).sum()
    }

    /// Checks the document against the config types, the same way `mirrord exec` reads the file:
    /// unknown keys and wrong types are reported, anything that needs the cluster or the
    /// environment is not.
    pub fn check(&self) -> Result<(), String> {
        serde_json::from_value::<LayerFileConfig>(Value::Object(self.root.clone()))
            .map(drop)
            .map_err(|error| error.to_string())
    }

    /// Writes the file, then runs it through the same resolution and checks
    /// `mirrord verify-config` does, so what was saved is known to be usable.
    ///
    /// Returns the error from either step. A file that saved but did not verify is still saved:
    /// the user may be halfway through a change that needs several options.
    pub fn save(&mut self) -> Result<(), SaveError> {
        if let Some(reason) = &self.unreadable {
            return Err(SaveError::Unreadable(reason.clone()));
        }

        let text = self.format.render(&self.root).map_err(SaveError::Write)?;
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| SaveError::Write(error.to_string()))?;
        }
        std::fs::write(&self.path, text).map_err(|error| SaveError::Write(error.to_string()))?;
        self.dirty = false;
        self.has_comments = false;

        // An empty target is what `mirrord exec` without `-t` runs with: no target.
        let mut context = ConfigContext::default()
            .empty_target_final(true)
            .override_env(LayerConfig::FILE_PATH_ENV, &self.path);
        LayerConfig::resolve(&mut context)
            .and_then(|config| config.verify(&mut context))
            .map_err(|error| SaveError::Rejected(error.to_string()))
    }
}

/// Why [`Document::save`] did not end with a usable file.
#[derive(Debug)]
pub enum SaveError {
    /// Saving was refused, to keep a file the screen could not read.
    Unreadable(String),
    /// The file could not be written.
    Write(String),
    /// The file was written, and mirrord rejects it.
    Rejected(String),
}

/// Removes keys set to `null`, at any depth. mirrord reads them as not set, which is what
/// leaving them out says.
fn drop_nulls(object: &mut Map<String, Value>) {
    object.retain(|_, value| !value.is_null());
    for value in object.values_mut() {
        if let Value::Object(child) = value {
            drop_nulls(child);
        }
    }
}

/// Removes `path` from `object`, then drops parents it left empty. Returns whether anything was
/// removed.
fn remove(object: &mut Map<String, Value>, path: &[String]) -> bool {
    let Some((first, rest)) = path.split_first() else {
        return false;
    };
    if rest.is_empty() {
        return object.remove(first).is_some();
    }

    let Some(Value::Object(child)) = object.get_mut(first) else {
        return false;
    };
    let removed = remove(child, rest);
    if child.is_empty() {
        object.remove(first);
    }
    removed
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::Document;

    fn path(keys: &[&str]) -> Vec<String> {
        keys.iter().map(|key| (*key).to_owned()).collect()
    }

    fn empty() -> Document {
        Document::load(&std::env::temp_dir().join("mirrord-tui-no-such-dir"))
    }

    #[test]
    fn set_builds_sections_and_unset_prunes_them() {
        let mut document = empty();
        document.set(&path(&["feature", "network", "incoming"]), json!("steal"));
        document.set(&path(&["feature", "fs"]), json!("read"));
        assert_eq!(document.set_count(), 2);
        assert!(document.is_dirty());

        // A mode string gives way to a settings object when a setting under it is set.
        document.set(
            &path(&["feature", "network", "incoming", "mode"]),
            json!("steal"),
        );
        assert_eq!(
            document.get(&path(&["feature", "network", "incoming"])),
            Some(&json!({ "mode": "steal" }))
        );

        document.unset(&path(&["feature", "network", "incoming", "mode"]));
        document.unset(&path(&["feature", "fs"]));
        assert_eq!(document.set_count(), 0);
        assert_eq!(document.get(&path(&["feature"])), None);
    }

    /// Writes `text` as `.mirrord/<name>` under a fresh directory and loads it.
    fn load_file(name: &str, text: &str) -> (std::path::PathBuf, Document) {
        let dir =
            std::env::temp_dir().join(format!("mirrord-tui-doc-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".mirrord")).unwrap();
        std::fs::write(dir.join(".mirrord").join(name), text).unwrap();
        let document = Document::load(&dir);
        (dir, document)
    }

    #[test]
    fn yaml_and_toml_files_round_trip() {
        let (dir, mut document) = load_file(
            "mirrord.yaml",
            "# team defaults\nfeature:\n  network:\n    incoming: steal\n  fs: ~\n",
        );
        assert_eq!(document.display_path(), ".mirrord/mirrord.yaml");
        assert!(document.has_comments());
        // `fs: ~` is null, which mirrord reads as not set.
        assert_eq!(document.set_count(), 1);
        document.set(&path(&["feature", "fs"]), json!("read"));
        let _ = document.save();
        let saved: serde_json::Value = serde_saphyr::from_str(
            &std::fs::read_to_string(dir.join(".mirrord/mirrord.yaml")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            saved,
            json!({ "feature": { "network": { "incoming": "steal" }, "fs": "read" } })
        );

        let (dir, mut document) =
            load_file("mirrord.toml", "[feature.network]\nincoming = \"mirror\"\n");
        assert_eq!(document.display_path(), ".mirrord/mirrord.toml");
        assert!(!document.has_comments());
        // A plain value next to a section, which TOML has to write before the section.
        document.set(&path(&["accept_invalid_certificates"]), json!(true));
        let _ = document.save();
        let saved: serde_json::Value =
            toml::from_str(&std::fs::read_to_string(dir.join(".mirrord/mirrord.toml")).unwrap())
                .unwrap();
        assert_eq!(
            saved,
            json!({ "accept_invalid_certificates": true, "feature": { "network": { "incoming": "mirror" } } })
        );
    }

    #[test]
    fn a_file_it_cannot_read_is_never_overwritten() {
        let (dir, mut document) =
            load_file("mirrord.json", "{ \"target\": {{ get_env(name=\"T\") }} }");
        assert!(document.unreadable().is_some());
        assert!(document.save().is_err());
        assert!(
            std::fs::read_to_string(dir.join(".mirrord/mirrord.json"))
                .unwrap()
                .contains("get_env")
        );
    }

    #[test]
    fn check_reports_what_mirrord_would_reject() {
        let mut document = empty();
        document.set(&path(&["feature", "network", "incoming"]), json!("steal"));
        assert_eq!(document.check(), Ok(()));

        document.set(&path(&["feature", "network", "incoming"]), json!("stael"));
        assert!(document.check().is_err());

        document.set(&path(&["no_such_option"]), json!(true));
        assert!(document.check().is_err());
    }
}
