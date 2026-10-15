//! How an option's value is written out for the one-line editor, and how what the user typed is
//! read back.
//!
//! Plain values are typed as they are (`steal`, `8080`). Lists of plain values are comma
//! separated (`80, 443`), and port pairs use a colon (`80:8080, 443:8443`), the way people
//! write them anyway. Everything else is typed as JSON, which keeps every option editable even
//! when it has no friendlier form yet.

use serde_json::{Number, Value};

use crate::config_schema::Shape;

/// Separates list items in the editor.
const ITEM_SEPARATOR: &str = ", ";

/// Separates the positions of a pair in the editor.
const PAIR_SEPARATOR: char = ':';

/// The text the editor starts with for `value`.
pub fn to_text(shape: &Shape, value: Option<&Value>) -> String {
    let Some(value) = value else {
        return String::new();
    };

    match (shape, value) {
        (Shape::List(item), Value::Array(items)) if is_plain(item) || is_pair(item) => items
            .iter()
            .map(|item_value| to_text(item, Some(item_value)))
            .collect::<Vec<_>>()
            .join(ITEM_SEPARATOR),
        (Shape::Tuple(shapes), Value::Array(items)) if shapes.iter().all(is_plain) => items
            .iter()
            .map(plain_text)
            .collect::<Vec<_>>()
            .join(&PAIR_SEPARATOR.to_string()),
        (shape, value) if is_plain(shape) => plain_text(value),
        (_, value) => value.to_string(),
    }
}

/// Reads what the user typed. Empty text means "unset", returned as `None`.
pub fn from_text(shape: &Shape, text: &str) -> Result<Option<Value>, String> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }

    let value = match shape {
        Shape::List(item) if is_plain(item) || is_pair(item) => Value::Array(
            text.split(',')
                .map(str::trim)
                .filter(|item_text| !item_text.is_empty())
                .map(|item_text| parse(item, item_text))
                .collect::<Result<_, _>>()?,
        ),
        shape => parse(shape, text)?,
    };
    Ok(Some(value))
}

/// Reads one value of `shape` from non-empty `text`.
fn parse(shape: &Shape, text: &str) -> Result<Value, String> {
    match shape {
        Shape::Text => Ok(Value::String(text.to_owned())),
        Shape::Choice(options) if options.iter().any(|option| option == text) => {
            Ok(Value::String(text.to_owned()))
        }
        Shape::Choice(options) => Err(format!("`{text}` is not one of: {}", options.join(", "))),
        Shape::Toggle => match text {
            "true" | "on" | "yes" => Ok(Value::Bool(true)),
            "false" | "off" | "no" => Ok(Value::Bool(false)),
            _ => Err(format!("`{text}` is not on or off")),
        },
        Shape::Integer { min, max } => {
            let number: i64 = text
                .parse()
                .map_err(|_| format!("`{text}` is not a whole number"))?;
            if let Some(min) = min
                && number < *min
            {
                return Err(format!("{number} is below the lowest allowed value, {min}"));
            }
            if let Some(max) = max
                && number > *max
            {
                return Err(format!(
                    "{number} is above the highest allowed value, {max}"
                ));
            }
            Ok(Value::from(number))
        }
        Shape::Number => text
            .parse::<f64>()
            .ok()
            .and_then(Number::from_f64)
            .map(Value::Number)
            .ok_or_else(|| format!("`{text}` is not a number")),
        Shape::Tuple(shapes) if shapes.iter().all(is_plain) => {
            let parts: Vec<&str> = text.split(PAIR_SEPARATOR).map(str::trim).collect();
            if parts.len() != shapes.len() {
                return Err(format!(
                    "`{text}` needs {} parts separated by `{PAIR_SEPARATOR}`",
                    shapes.len()
                ));
            }
            shapes
                .iter()
                .zip(parts)
                .map(|(shape, part)| parse(shape, part))
                .collect::<Result<_, _>>()
                .map(Value::Array)
        }
        _ => serde_json::from_str(text).map_err(|error| format!("not valid JSON: {error}")),
    }
}

/// A value typed as itself rather than as JSON.
fn is_plain(shape: &Shape) -> bool {
    matches!(
        shape,
        Shape::Text | Shape::Choice(_) | Shape::Toggle | Shape::Integer { .. } | Shape::Number
    )
}

fn is_pair(shape: &Shape) -> bool {
    matches!(shape, Shape::Tuple(shapes) if shapes.iter().all(is_plain))
}

/// A plain value as typed: strings without their quotes.
fn plain_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        value => value.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{from_text, to_text};
    use crate::config_schema::Shape;

    fn port() -> Shape {
        Shape::Integer {
            min: Some(0),
            max: Some(65535),
        }
    }

    #[test]
    fn plain_values_round_trip() {
        let mode = Shape::Choice(vec!["mirror".to_owned(), "steal".to_owned()]);
        assert_eq!(to_text(&mode, Some(&json!("steal"))), "steal");
        assert_eq!(from_text(&mode, " steal "), Ok(Some(json!("steal"))));
        assert!(from_text(&mode, "stael").is_err());

        assert_eq!(from_text(&port(), "8080"), Ok(Some(json!(8080))));
        assert!(from_text(&port(), "70000").unwrap_err().contains("65535"));
        assert!(from_text(&port(), "eighty").is_err());

        // Empty means "back to mirrord's default".
        assert_eq!(from_text(&Shape::Text, "   "), Ok(None));
    }

    #[test]
    fn lists_and_port_pairs_are_typed_the_way_people_write_them() {
        let ports = Shape::List(Box::new(port()));
        assert_eq!(to_text(&ports, Some(&json!([80, 443]))), "80, 443");
        assert_eq!(from_text(&ports, "80, 443,"), Ok(Some(json!([80, 443]))));

        let mapping = Shape::List(Box::new(Shape::Tuple(vec![port(), port()])));
        assert_eq!(
            to_text(&mapping, Some(&json!([[80, 8080], [443, 8443]]))),
            "80:8080, 443:8443"
        );
        assert_eq!(
            from_text(&mapping, "80:8080, 443 : 8443"),
            Ok(Some(json!([[80, 8080], [443, 8443]])))
        );
        assert!(from_text(&mapping, "80").unwrap_err().contains("2 parts"));
    }

    #[test]
    fn everything_else_is_json() {
        let env = Shape::Map(Box::new(Shape::Text));
        assert_eq!(to_text(&env, Some(&json!({ "A": "1" }))), r#"{"A":"1"}"#);
        assert_eq!(
            from_text(&env, r#"{"A": "1"}"#),
            Ok(Some(json!({ "A": "1" })))
        );
        assert!(
            from_text(&env, "{oops")
                .unwrap_err()
                .starts_with("not valid JSON")
        );
    }
}
