//! The mirrord config file's options, read from its JSON schema.
//!
//! The Config screen draws and edits every option from what this module returns, so an option
//! added to `mirrord-config` shows up in the TUI with no TUI change. The schema comes from
//! [`LayerFileConfig::schema`], built from the same types the CLI parses, so the screen never
//! offers an option this build would reject.
//!
//! The schema is reduced to a handful of [`Shape`]s, one per kind of editor. `null` is never a
//! shape of its own: every option can be left out of the file, and a left-out option is how the
//! screen shows "not set".

use std::sync::LazyLock;

use mirrord_config::{LayerFileConfig, plan::Plan};
use serde::Deserialize;
use serde_json::Value;

/// Where the schema records which plan an option needs.
const PLAN_ANNOTATION: &str = "x-mirrord-plan";

/// Marks doc text that the published docs leave out. The text itself is still worth reading in
/// the TUI, so only the marker is dropped.
const INTERNAL_MARKER: &str = "<!--${internal}-->";

/// Every top-level option of the config file, built once per process.
pub static FIELDS: LazyLock<Vec<Field>> = LazyLock::new(|| fields_of(&LayerFileConfig::schema()));

/// One option of the config file.
#[derive(Debug)]
pub struct Field {
    /// The key it has in its parent object.
    pub key: String,
    /// The option's documentation, as written on the config type.
    pub doc: Option<String>,
    /// The plan it needs, inherited from the enclosing option when it does not say.
    pub plan: Option<Plan>,
    pub shape: Shape,
}

/// What kind of value an option takes, and so which editor it gets.
#[derive(Debug)]
pub enum Shape {
    Toggle,
    /// One of a fixed set of strings.
    Choice(Vec<String>),
    Text,
    Integer {
        min: Option<i64>,
        max: Option<i64>,
    },
    Number,
    List(Box<Shape>),
    /// An object with free-form keys, such as env overrides.
    Map(Box<Shape>),
    /// An object with fixed keys: a section of the config.
    Group(Vec<Field>),
    /// A fixed-length list whose positions mean different things, such as a `[local, remote]`
    /// port pair.
    Tuple(Vec<Shape>),
    /// Different shapes the same option accepts, such as `incoming` being either a mode string
    /// or an object of settings.
    Variants(Vec<Variant>),
    /// A value the schema deliberately leaves open, edited as raw JSON.
    Free,
    /// A schema this module does not understand, also edited as raw JSON. The tests keep the
    /// real config free of these.
    Json,
}

/// One of the shapes a [`Shape::Variants`] option accepts.
#[derive(Debug)]
pub struct Variant {
    /// How the shape is named when the user picks between them.
    pub label: String,
    pub shape: Shape,
}

impl Shape {
    /// Whether `value` is a value of this shape, used to tell which variant a value is.
    pub fn accepts(&self, value: &Value) -> bool {
        match (self, value) {
            (Self::Toggle, Value::Bool(_)) => true,
            (Self::Choice(options), Value::String(value)) => options.contains(value),
            (Self::Text, Value::String(_)) => true,
            (Self::Integer { .. }, Value::Number(number)) => number.is_i64() || number.is_u64(),
            (Self::Number, Value::Number(_)) => true,
            (Self::List(_), Value::Array(_)) => true,
            (Self::Tuple(shapes), Value::Array(items)) => shapes.len() == items.len(),
            (Self::Map(_) | Self::Group(_), Value::Object(_)) => true,
            (Self::Variants(variants), value) => {
                variants.iter().any(|variant| variant.shape.accepts(value))
            }
            (Self::Free | Self::Json, _) => true,
            _ => false,
        }
    }

    /// The variant `value` is a value of, the first match winning.
    pub fn active_variant(&self, value: &Value) -> Option<&Variant> {
        let Self::Variants(variants) = self else {
            return None;
        };
        variants.iter().find(|variant| variant.shape.accepts(value))
    }

    /// A short description of the values this shape takes, for the details panel.
    pub fn describe(&self) -> String {
        match self {
            Self::Toggle => "on or off".to_owned(),
            Self::Choice(options) => format!("one of: {}", options.join(", ")),
            Self::Text => "text".to_owned(),
            Self::Integer {
                min: Some(min),
                max: Some(max),
            } => format!("whole number from {min} to {max}"),
            Self::Integer { min: Some(min), .. } => format!("whole number, at least {min}"),
            Self::Integer { .. } => "whole number".to_owned(),
            Self::Number => "number".to_owned(),
            Self::List(item) => format!("list of {}", item.describe()),
            Self::Map(value) => format!("map of names to {}", value.describe()),
            Self::Tuple(shapes) => shapes
                .iter()
                .map(Shape::describe)
                .collect::<Vec<_>>()
                .join(" : "),
            Self::Group(fields) => format!("section with {} options", fields.len()),
            Self::Variants(variants) => variants
                .iter()
                .map(|variant| variant.label.as_str())
                .collect::<Vec<_>>()
                .join(", or "),
            Self::Free | Self::Json => "any JSON value".to_owned(),
        }
    }
}

/// The fields of the object schema `schema`, resolving references against `root`.
pub fn fields_of(root: &Value) -> Vec<Field> {
    Reader { root }.fields(root, None, &mut Vec::new())
}

/// Walks one schema document. `refs` holds the `$ref`s being expanded on the way down, so a type
/// that contains itself is cut off as [`Shape::Json`] instead of looping.
struct Reader<'a> {
    root: &'a Value,
}

impl<'a> Reader<'a> {
    fn target(&self, reference: &str) -> Option<&'a Value> {
        reference
            .strip_prefix('#')
            .and_then(|pointer| self.root.pointer(pointer))
    }

    fn fields(&self, object: &'a Value, plan: Option<Plan>, refs: &mut Vec<&'a str>) -> Vec<Field> {
        let Some(properties) = object.get("properties").and_then(Value::as_object) else {
            return Vec::new();
        };

        properties
            .iter()
            .map(|(key, schema)| {
                let plan = self.plan(schema).or(plan);
                Field {
                    key: key.clone(),
                    doc: self.doc(schema),
                    plan,
                    shape: self.shape(schema, plan, refs),
                }
            })
            .collect()
    }

    /// The schemas `schema` stands for: itself, then everything its `$ref`s and alternatives
    /// lead to, outermost first, so an option's own docs come before those of its type. Docs and
    /// plans can sit on any of them.
    fn layers(&self, schema: &'a Value) -> Vec<&'a Value> {
        let mut layers = vec![schema];
        let mut seen = Vec::new();
        let mut index = 0;
        while let Some(current) = layers.get(index).copied() {
            index += 1;
            if let Some(reference) = current.get("$ref").and_then(Value::as_str)
                && !seen.contains(&reference)
            {
                seen.push(reference);
                layers.extend(self.target(reference));
            }
            layers.extend(
                branches(current)
                    .into_iter()
                    .filter(|branch| !is_null(branch)),
            );
        }
        layers
    }

    fn doc(&self, schema: &'a Value) -> Option<String> {
        self.layers(schema).into_iter().find_map(|layer| {
            let text = layer.get("description")?.as_str()?;
            let text = text.replace(INTERNAL_MARKER, "");
            let text = text.trim();
            (!text.is_empty()).then(|| text.to_owned())
        })
    }

    fn plan(&self, schema: &'a Value) -> Option<Plan> {
        self.layers(schema).into_iter().find_map(|layer| {
            layer
                .get(PLAN_ANNOTATION)
                .and_then(|plan| Plan::deserialize(plan).ok())
        })
    }

    fn shape(&self, schema: &'a Value, plan: Option<Plan>, refs: &mut Vec<&'a str>) -> Shape {
        if let Some(reference) = schema.get("$ref").and_then(Value::as_str) {
            if refs.contains(&reference) {
                return Shape::Json;
            }
            let Some(target) = self.target(reference) else {
                return Shape::Json;
            };
            refs.push(reference);
            let shape = self.shape(target, self.plan(target).or(plan), refs);
            refs.pop();
            return shape;
        }

        let branches = branches(schema);
        if !branches.is_empty() {
            let shapes = branches
                .into_iter()
                .filter(|branch| !is_null(branch))
                .map(|branch| (self.label_hint(branch), self.shape(branch, plan, refs)))
                .collect();
            return merge(shapes);
        }

        // Kubernetes types mark their free-form parts this way instead of with a JSON type.
        if schema.get("x-kubernetes-preserve-unknown-fields").is_some() {
            return Shape::Free;
        }
        if schema.get("x-kubernetes-int-or-string").is_some() {
            return Shape::Text;
        }

        if let Some(value) = schema.get("const").and_then(Value::as_str) {
            return Shape::Choice(vec![value.to_owned()]);
        }
        if let Some(options) = schema.get("enum").and_then(Value::as_array) {
            return Shape::Choice(
                options
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect(),
            );
        }

        let types: Vec<&str> = match schema.get("type") {
            Some(Value::String(kind)) => vec![kind.as_str()],
            Some(Value::Array(kinds)) => kinds
                .iter()
                .filter_map(Value::as_str)
                .filter(|kind| *kind != "null")
                .collect(),
            _ => Vec::new(),
        };

        match types.as_slice() {
            // `{}` accepts anything on purpose.
            [] if schema.as_object().is_some_and(|object| {
                object
                    .keys()
                    .all(|key| matches!(key.as_str(), "description" | "title" | "default"))
            }) =>
            {
                Shape::Free
            }
            [] => Shape::Json,
            [kind] => self.typed(schema, kind, plan, refs),
            kinds => merge(
                kinds
                    .iter()
                    .map(|kind| (None, self.typed(schema, kind, plan, refs)))
                    .collect(),
            ),
        }
    }

    fn typed(
        &self,
        schema: &'a Value,
        kind: &str,
        plan: Option<Plan>,
        refs: &mut Vec<&'a str>,
    ) -> Shape {
        match kind {
            "boolean" => Shape::Toggle,
            "string" => Shape::Text,
            "integer" => Shape::Integer {
                min: schema.get("minimum").and_then(Value::as_i64),
                max: schema.get("maximum").and_then(Value::as_i64),
            },
            "number" => Shape::Number,
            "array" => match schema.get("prefixItems").and_then(Value::as_array) {
                Some(positions) => Shape::Tuple(
                    positions
                        .iter()
                        .map(|position| self.shape(position, plan, refs))
                        .collect(),
                ),
                None => Shape::List(Box::new(match schema.get("items") {
                    Some(items) => self.shape(items, plan, refs),
                    None => Shape::Free,
                })),
            },
            "object"
                if schema.get("properties").is_some()
                    || schema.get("additionalProperties") == Some(&Value::Bool(false)) =>
            {
                Shape::Group(self.fields(schema, plan, refs))
            }
            "object" => Shape::Map(Box::new(match schema.get("additionalProperties") {
                Some(value @ Value::Object(_)) => self.shape(value, plan, refs),
                _ => Shape::Free,
            })),
            _ => Shape::Json,
        }
    }

    /// The name of the type a branch refers to, turned into words for a variant label:
    /// `#/$defs/IncomingAdvancedFileConfig` becomes `incoming advanced settings`.
    fn label_hint(&self, branch: &Value) -> Option<String> {
        let name = branch.get("$ref")?.as_str()?.rsplit('/').next()?;
        let name = name
            .strip_suffix("FileConfig")
            .or_else(|| name.strip_suffix("Config"))
            .unwrap_or(name);
        let mut words = String::new();
        for char in name.chars() {
            if char.is_uppercase() && !words.is_empty() {
                words.push(' ');
            }
            words.extend(char.to_lowercase());
        }
        Some(words)
    }
}

/// The alternatives a schema offers, through `anyOf`, `oneOf` or `allOf`.
fn branches(schema: &Value) -> Vec<&Value> {
    ["anyOf", "oneOf", "allOf"]
        .into_iter()
        .filter_map(|keyword| schema.get(keyword).and_then(Value::as_array))
        .flatten()
        .collect()
}

fn is_null(schema: &Value) -> bool {
    schema.get("type").and_then(Value::as_str) == Some("null")
}

/// Turns the shapes of a schema's alternatives into one shape: choices are pooled into a single
/// choice, nested alternatives are flattened, and a lone shape is just that shape.
fn merge(shapes: Vec<(Option<String>, Shape)>) -> Shape {
    let mut variants: Vec<Variant> = Vec::new();

    for (hint, shape) in shapes {
        match shape {
            Shape::Variants(nested) => variants.extend(nested),
            Shape::Choice(options) => {
                match variants
                    .iter_mut()
                    .find_map(|variant| match &mut variant.shape {
                        Shape::Choice(existing) => Some(existing),
                        _ => None,
                    }) {
                    Some(existing) => {
                        for option in options {
                            if !existing.contains(&option) {
                                existing.push(option);
                            }
                        }
                    }
                    None => variants.push(Variant {
                        label: String::new(),
                        shape: Shape::Choice(options),
                    }),
                }
            }
            shape => variants.push(Variant {
                label: hint.unwrap_or_default(),
                shape,
            }),
        }
    }

    // A choice offered next to free text is one of the suggested values of that text, not a
    // separate shape: `target` accepts any path, and names some.
    if variants
        .iter()
        .any(|variant| matches!(variant.shape, Shape::Text))
    {
        variants.retain(|variant| !matches!(variant.shape, Shape::Choice(_)));
    }

    if variants.len() == 1 {
        return variants.pop().map_or(Shape::Json, |variant| variant.shape);
    }
    for variant in &mut variants {
        if variant.label.is_empty() {
            variant.label = default_label(&variant.shape);
        }
    }
    Shape::Variants(variants)
}

/// A variant's label when the schema gives no type name to build one from.
fn default_label(shape: &Shape) -> String {
    match shape {
        Shape::Toggle => "on or off".to_owned(),
        Shape::Choice(options) => options.join(" / "),
        Shape::Text => "text".to_owned(),
        Shape::Integer { .. } | Shape::Number => "number".to_owned(),
        Shape::List(_) => "list".to_owned(),
        Shape::Tuple(_) => "pair".to_owned(),
        Shape::Map(_) => "map".to_owned(),
        Shape::Group(_) => "settings".to_owned(),
        Shape::Variants(_) | Shape::Free | Shape::Json => "JSON".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{FIELDS, Field, Shape, fields_of};

    fn find<'a>(fields: &'a [Field], path: &[&str]) -> &'a Field {
        let (first, rest) = path
            .split_first()
            .expect("paths in these tests are never empty");
        let field = fields
            .iter()
            .find(|field| field.key == *first)
            .unwrap_or_else(|| panic!("no option {first}"));
        if rest.is_empty() {
            return field;
        }
        let children = match &field.shape {
            Shape::Group(children) => children,
            Shape::Variants(variants) => variants
                .iter()
                .find_map(|variant| match &variant.shape {
                    Shape::Group(children) => Some(children),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("{first} has no settings variant")),
            other => panic!("{first} is {other:?}, not a section"),
        };
        find(children, rest)
    }

    /// Every option of the real config gets an editor of its own: anything that fell back to raw
    /// JSON here is a schema shape this module does not understand yet.
    #[test]
    fn every_config_option_gets_an_editor() {
        fn walk(fields: &[Field], path: &str, raw: &mut Vec<String>) {
            for field in fields {
                let path = format!("{path}{}", field.key);
                check(&field.shape, &path, raw);
            }
        }
        fn check(shape: &Shape, path: &str, raw: &mut Vec<String>) {
            match shape {
                Shape::Group(fields) => walk(fields, &format!("{path}."), raw),
                Shape::Variants(variants) => {
                    for variant in variants {
                        check(&variant.shape, path, raw);
                    }
                }
                Shape::List(item) | Shape::Map(item) => check(item, path, raw),
                Shape::Tuple(shapes) => {
                    for shape in shapes {
                        check(shape, path, raw);
                    }
                }
                Shape::Json => raw.push(path.to_owned()),
                _ => {}
            }
        }

        let mut raw = Vec::new();
        walk(&FIELDS, "", &mut raw);
        assert!(FIELDS.len() > 20, "only {} top-level options", FIELDS.len());
        assert!(
            raw.is_empty(),
            "options with no editor of their own: {raw:#?}"
        );
    }

    #[test]
    fn incoming_is_a_mode_or_settings() {
        let incoming = find(&FIELDS, &["feature", "network", "incoming"]);
        let Shape::Variants(variants) = &incoming.shape else {
            panic!("incoming is {:?}", incoming.shape);
        };
        assert!(variants.iter().any(|variant| matches!(
            &variant.shape,
            Shape::Choice(options) if options.iter().any(|option| option == "steal")
        )));
        assert!(
            variants
                .iter()
                .any(|variant| matches!(variant.shape, Shape::Group(_)))
        );
        assert!(
            incoming
                .doc
                .as_deref()
                .is_some_and(|doc| doc.contains("incoming"))
        );

        let mode = find(&FIELDS, &["feature", "network", "incoming", "mode"]);
        assert!(matches!(mode.shape, Shape::Choice(_)), "{:?}", mode.shape);
    }

    #[test]
    fn nullable_types_refs_and_plans() {
        let schema = json!({
            "properties": {
                "flag": { "type": ["boolean", "null"], "description": "<!--${internal}--> A flag." },
                "port": { "type": ["integer", "null"], "minimum": 0, "maximum": 65535 },
                "team": { "anyOf": [{ "$ref": "#/$defs/Team" }, { "type": "null" }] },
                "env": { "type": "object", "additionalProperties": { "type": "string" } }
            },
            "$defs": {
                "Team": {
                    "type": "object",
                    "x-mirrord-plan": "team",
                    "description": "Team settings.",
                    "properties": { "inner": { "type": "string" } }
                }
            }
        });
        let fields = fields_of(&schema);

        let flag = find(&fields, &["flag"]);
        assert!(matches!(flag.shape, Shape::Toggle));
        assert_eq!(flag.doc.as_deref(), Some("A flag."));

        assert!(matches!(
            find(&fields, &["port"]).shape,
            Shape::Integer {
                min: Some(0),
                max: Some(65535)
            }
        ));

        let team = find(&fields, &["team"]);
        assert_eq!(team.doc.as_deref(), Some("Team settings."));
        assert_eq!(team.plan, Some(mirrord_config::plan::Plan::Team));
        // Nested options inherit the plan of their section.
        assert_eq!(
            find(&fields, &["team", "inner"]).plan,
            Some(mirrord_config::plan::Plan::Team)
        );

        assert!(
            matches!(&find(&fields, &["env"]).shape, Shape::Map(value) if matches!(**value, Shape::Text))
        );
    }
}
