use std::{ops::Deref, time::Duration};

use thiserror::Error;
use tokio::task::spawn_blocking;

#[derive(Error, Debug)]
pub enum JqError {
    #[error("jq filter could not be parsed. Code: {jq_code}.{}", if let Some(error) = error { "Parse error: {error}" } else { "" })]
    Load {
        jq_code: String,
        error: Option<String>,
    },
    #[error("jq filter does not compile. Code: {jq_code}. Compile error: {error}")]
    Compile { jq_code: String, error: String },
    #[error("jq filter evaluation failed. Code: {jq_code}. Input: {input}. Error: {error}")]
    Evaluate {
        jq_code: String,
        input: serde_json::Value,
        error: String,
    },
    #[error(
        "jq filter evaluation timed out. Code: {jq_code}. Input: {input}. Timeout: {timeout:?}"
    )]
    Timeout {
        jq_code: String,
        input: serde_json::Value,
        timeout: Duration,
    },
}

pub type Result<T> = std::result::Result<T, JqError>;

type CompileError<'a> = Vec<(
    jaq_core::load::File<&'a str, ()>,
    Vec<(&'a str, jaq_core::compile::Undefined)>,
)>;

const FILE_PREFIX: &str = "In code: ";
const UNDEFINED_PREFIX: &str = "Undefined ";
const BETWEEN_SYMBOL_TYPE_AND_IDENTIFIER: &str = ": ";

const fn symbol_type_name_len(symbol_type: &jaq_core::compile::Undefined) -> usize {
    match symbol_type {
        jaq_core::compile::Undefined::Var => "variable".len(),
        jaq_core::compile::Undefined::Mod => "module".len(),
        jaq_core::compile::Undefined::Label => "label".len(),
        jaq_core::compile::Undefined::Filter(_) => "filter".len(),
        _ => 8, // arbitrary capacity for future symbol types.
    }
}

// dumb optimization just for fun: avoiding unnecessary allocations when creating
// the compile error string by estimating the final string length and pre-allocating
// a String with that capacity.
fn estimated_string_len(error: &CompileError) -> usize {
    let mut capacity = FILE_PREFIX.len() * error.len();
    capacity += error.len(); // for the newline after the file code.
    for (file, undefineds) in error {
        capacity += file.code.len();
        capacity += UNDEFINED_PREFIX.len() * undefineds.len();
        capacity += BETWEEN_SYMBOL_TYPE_AND_IDENTIFIER.len() * undefineds.len();
        capacity += undefineds.len(); // for the newline after each undefined symbol.
        for (identifier, symbol_type) in undefineds {
            capacity += symbol_type_name_len(symbol_type);
            capacity += identifier.len();
        }
    }
    capacity
}

fn compiler_error_to_string(error: CompileError) -> String {
    let capacity = estimated_string_len(&error);
    let mut compile_error = String::with_capacity(capacity);
    for (file, undefineds) in error {
        compile_error += FILE_PREFIX;
        compile_error += file.code;
        compile_error.push('\n');
        for (identifier, symbol_type) in undefineds {
            compile_error += UNDEFINED_PREFIX;
            compile_error += symbol_type.as_str();
            compile_error += BETWEEN_SYMBOL_TYPE_AND_IDENTIFIER;
            compile_error += identifier;
            compile_error.push('\n');
        }
    }
    compile_error
}

pub fn compile_jq(code: &str) -> Result<jaq_core::Filter<jaq_core::Native<jaq_json::Val>>> {
    let file = jaq_core::load::File { code, path: () };
    let loader = jaq_core::load::Loader::new(jaq_std::defs().chain(jaq_json::defs()));
    let arena = jaq_core::load::Arena::default();
    let modules = loader.load(&arena, file).map_err(|errors| {
        // Since we only load 1 file, there should only be 1 error, take first.
        JqError::Load {
            jq_code: code.to_string(),
            error: errors.first().map(|err| format!("{:?}", err.1)),
        }
    })?;

    let filter = jaq_core::Compiler::default().with_funs(jaq_std::funs().chain(jaq_json::funs()));

    // Convert the compile errors into a human-readable string using our helper.
    filter.compile(modules).map_err(|errors| JqError::Compile {
        jq_code: code.to_string(),
        error: compiler_error_to_string(errors),
    })
}

pub async fn evaluate_jq(
    jq_code: &str,
    payload: &serde_json::Value,
    timeout_duration: Duration,
) -> Result<bool> {
    let inputs = jaq_core::RcIter::new(core::iter::empty());
    let filter = compile_jq(jq_code)?;
    let owned_json_value = payload.clone();
    let jaq_run_handle = spawn_blocking(move || {
        let mut out = filter.run((
            jaq_core::Ctx::new([], &inputs),
            jaq_json::Val::from(owned_json_value),
        ));
        out.find_map(|item| {
            if let Ok(jaq_json::Val::Bool(value)) = &item {
                Some(*value)
            } else {
                None
            }
        })
        .unwrap_or(false)
    });

    match tokio::time::timeout(timeout_duration, jaq_run_handle).await {
        // timed out while waiting for the spawned blocking task
        Err(..) => Err(JqError::Timeout {
            jq_code: jq_code.to_string(),
            input: payload.clone(),
            timeout: timeout_duration,
        }),
        // the spawned task panicked or the join failed for some reason
        Ok(Err(err)) => Err(JqError::Evaluate {
            jq_code: jq_code.to_string(),
            input: payload.clone(),
            error: format!("jq program execution failed: {err:?}"),
        }),
        // successful execution
        Ok(Ok(found_match)) => Ok(found_match),
    }
}

/// A string-wrapper that can only be constructed with a string that is a valid jq program.
#[derive(Debug, Clone)]
pub struct VerifiedJqString(String);

impl From<VerifiedJqString> for String {
    fn from(value: VerifiedJqString) -> Self {
        value.0
    }
}

impl TryFrom<String> for VerifiedJqString {
    type Error = JqError;

    fn try_from(jq_code: String) -> std::result::Result<Self, Self::Error> {
        compile_jq(&jq_code).map(|_| Self(jq_code))
    }
}

impl TryFrom<&str> for VerifiedJqString {
    type Error = JqError;

    fn try_from(jq_code: &str) -> std::result::Result<Self, Self::Error> {
        compile_jq(jq_code).map(|_| Self(jq_code.to_string()))
    }
}

impl Deref for VerifiedJqString {
    type Target = String;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl AsRef<str> for VerifiedJqString {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use jaq_core::compile::Undefined;

    use super::*;

    #[test]
    fn jq_verification_valid_programs() {
        VerifiedJqString::try_from(".snow").unwrap();
        VerifiedJqString::try_from("{snow, wind}").unwrap();
        VerifiedJqString::try_from(".[]").unwrap();
        VerifiedJqString::try_from(".[] | select(.snow > 25)").unwrap();
    }

    #[test]
    fn jq_verification_fails_on_invalid_programs() {
        VerifiedJqString::try_from("snow").unwrap_err();
        VerifiedJqString::try_from("").unwrap_err();
        VerifiedJqString::try_from("idk | whatever").unwrap_err();
    }

    #[test]
    fn test_estimate_string_len() {
        // This error doesn't make sens (complains about undefined symbols that don't appear in the
        // code) but that doesn't matter, we're just testing the string conversion and the
        // length estimation.
        let err = vec![
            (
                jaq_core::load::File {
                    code: "whatever whatever | whatever",
                    path: (),
                },
                vec![
                    ("var1", Undefined::Var),
                    ("mod1", Undefined::Mod),
                    ("lbl", Undefined::Label),
                    ("fltr", Undefined::Filter(0)),
                ],
            ),
            (
                jaq_core::load::File {
                    code: "some more jq code I guess",
                    path: (),
                },
                vec![("x", Undefined::Var)],
            ),
        ];

        let estimated = estimated_string_len(&err);
        let actual = compiler_error_to_string(err).len();
        assert_eq!(
            estimated, actual,
            "estimated capacity should match actual generated string length"
        );

        // Additional simple case: single file, single undefined
        let minimal_err = vec![(
            jaq_core::load::File {
                code: "wow | such jq | much filter",
                path: (),
            },
            vec![("tomato_counter", Undefined::Var)],
        )];

        assert_eq!(
            estimated_string_len(&minimal_err),
            compiler_error_to_string(minimal_err).len()
        );
    }
}
