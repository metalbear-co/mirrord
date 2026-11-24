use std::ops::Deref;

use thiserror::Error;

#[derive(Error, Debug)]
pub enum JqError {
    #[error("jq filter could not be parsed. Code: {jq_code}.{}", error.as_ref().map(|e| format!(" Parse error: {e}")).unwrap_or_default())]
    Load {
        jq_code: String,
        error: Option<String>,
    },
    #[error("jq filter does not compile. Code: {jq_code}. Compile error: {error}")]
    Compile { jq_code: String, error: String },
    #[cfg(feature = "eval")]
    #[error("jq filter evaluation failed. Code: {jq_code}. Input: {input}. Error: {error}")]
    Evaluate {
        jq_code: String,
        input: serde_json::Value,
        error: String,
    },
    #[cfg(feature = "eval")]
    #[error(
        "jq filter evaluation timed out. Code: {jq_code}. Input: {input}. Timeout: {timeout:?}"
    )]
    Timeout {
        jq_code: String,
        input: serde_json::Value,
        timeout: std::time::Duration,
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
            jq_code: code.to_owned(),
            error: errors.first().map(|err| format!("{:?}", err.1)),
        }
    })?;

    let filter = jaq_core::Compiler::default().with_funs(jaq_std::funs().chain(jaq_json::funs()));

    // Convert the compile errors into a human-readable string using our helper.
    filter.compile(modules).map_err(|errors| JqError::Compile {
        jq_code: code.to_owned(),
        error: compiler_error_to_string(errors),
    })
}

/// How long an evaluation waits for a blocking thread before it gives up. The pool normally
/// hands one over in microseconds, and a wait this long means the host is starved; failing then
/// keeps the caller from stalling without a trace.
#[cfg(feature = "eval")]
const THREAD_WAIT_LIMIT: std::time::Duration = std::time::Duration::from_secs(10);

/// Runs `jq_code` against `payload` and reports whether the program produced `true`.
///
/// `time_limit` bounds the program, not the runtime around it: the clock starts the instant a
/// blocking thread picks the evaluation up. Started at the call instead, the wait for that
/// thread counts against the budget, and on a busy host a program as small as
/// `.priority == "high"` reports [`JqError::Timeout`]. The operator treats that as a runaway
/// program: the message is routed as unmatched and the session that owns the filter is torn down.
///
/// A program that never finishes still fails with [`JqError::Timeout`] once the limit passes
/// from its start; its thread keeps running, as `spawn_blocking` cannot be cancelled. A pool
/// that hands out no thread within `THREAD_WAIT_LIMIT` fails the same way, so a starved host
/// shows up as an error rather than as a caller that waits forever.
#[cfg(feature = "eval")]
pub async fn evaluate_jq(
    jq_code: &str,
    payload: &serde_json::Value,
    time_limit: std::time::Duration,
) -> Result<bool> {
    let filter = compile_jq(jq_code)?;
    let owned_json_value = payload.clone();
    // Blocking worker --the instant it starts running the program--> task waiting in evaluate_jq.
    let (started_tx, started_rx) = tokio::sync::oneshot::channel::<tokio::time::Instant>();
    let jaq_run_handle = tokio::task::spawn_blocking(move || {
        // A receiver that is already gone only means the caller stopped waiting.
        let _ = started_tx.send(tokio::time::Instant::now());
        let inputs = jaq_core::RcIter::new(core::iter::empty());
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

    let started_at = match tokio::time::timeout(THREAD_WAIT_LIMIT, started_rx).await {
        Ok(Ok(started_at)) => started_at,
        // The worker was dropped before it ran, so the join below says how it ended.
        Ok(Err(_closed)) => tokio::time::Instant::now(),
        Err(_elapsed) => {
            return Err(JqError::Timeout {
                jq_code: jq_code.to_owned(),
                input: payload.clone(),
                timeout: THREAD_WAIT_LIMIT,
            });
        }
    };

    // The deadline counts from the worker's own start, so a caller that resumes late does not
    // hand the program extra time.
    match tokio::time::timeout_at(started_at + time_limit, jaq_run_handle).await {
        Err(..) => Err(JqError::Timeout {
            jq_code: jq_code.to_owned(),
            input: payload.clone(),
            timeout: time_limit,
        }),
        // the spawned task panicked or the join failed for some reason
        Ok(Err(err)) => Err(JqError::Evaluate {
            jq_code: jq_code.to_owned(),
            input: payload.clone(),
            error: format!("jq program execution failed: {err:?}"),
        }),
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
        compile_jq(jq_code).map(|_| Self(jq_code.to_owned()))
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
    #[cfg(feature = "eval")]
    use mirrord_test_macros::background_shutdown_tokio_test;

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
        let compile_error_string = compiler_error_to_string(err);
        println!("{compile_error_string}");
        let actual = compile_error_string.len();
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

    #[cfg(feature = "eval")]
    #[background_shutdown_tokio_test]
    #[timeout(std::time::Duration::from_secs(1))]
    async fn test_jq_evaluation_true() {
        let jq_code = "any(.[]; .snow > 25 and .wind > 10)";
        let payload = serde_json::json!([
            {"snow": 10, "wind": 5},
            {"snow": 30, "wind": 15},
            {"snow": 20, "wind": 10}
        ]);

        let result = evaluate_jq(jq_code, &payload, std::time::Duration::from_millis(500))
            .await
            .expect("JQ evaluation failed");
        assert!(
            result,
            "Wrong jq evaluation: expected to find a match but got false"
        );
    }

    #[cfg(feature = "eval")]
    #[background_shutdown_tokio_test]
    #[timeout(std::time::Duration::from_secs(1))]
    async fn test_jq_evaluation_false() {
        let jq_code = "any(.[]; .snow > 25 and .wind > 20)";
        let payload = serde_json::json!([
            {"snow": 10, "wind": 5},
            {"snow": 30, "wind": 15},
            {"snow": 20, "wind": 10}
        ]);

        let result = evaluate_jq(jq_code, &payload, std::time::Duration::from_millis(500))
            .await
            .expect("JQ evaluation failed");
        assert!(
            !result,
            "Wrong jq evaluation: expected not to find a match but got true"
        );
    }

    /// A busy blocking pool must not count against a program's time limit. With one blocking
    /// thread and that thread held past the limit, the evaluation starts late and used to report
    /// a timeout for a program that runs in microseconds; the operator then dropped the message
    /// and tore down the session that owned the filter.
    ///
    /// The thread is held until this test lets go, and the evaluation is checked to be pending
    /// past twice its limit first, so the test cannot pass by the evaluation sneaking in before
    /// the thread was taken.
    #[cfg(feature = "eval")]
    #[test]
    fn a_wait_for_a_blocking_thread_is_not_the_programs_time() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .max_blocking_threads(1)
            .build()
            .expect("runtime builds");

        runtime.block_on(async {
            let limit = std::time::Duration::from_millis(200);
            // Test --let go of the thread--> blocker holding the one blocking thread.
            let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
            let busy = tokio::task::spawn_blocking(move || {
                // Bounded so a failed assertion below does not hang the runtime's shutdown.
                let _ = release_rx.recv_timeout(std::time::Duration::from_secs(5));
            });

            let payload = serde_json::json!({"priority": "high"});
            let mut evaluation =
                std::pin::pin!(evaluate_jq(r#".priority == "high""#, &payload, limit));

            let early = tokio::time::timeout(limit * 2, evaluation.as_mut()).await;
            assert!(
                early.is_err(),
                "the evaluation must wait for the thread instead of ending, got {early:?}"
            );

            release_tx.send(()).expect("the blocker is waiting");
            let result = evaluation.await;
            assert!(
                matches!(result, Ok(true)),
                "the program must run once the thread frees up, got {result:?}"
            );
            busy.await.expect("the blocker finishes");
        });
    }

    #[cfg(feature = "eval")]
    #[background_shutdown_tokio_test]
    #[timeout(std::time::Duration::from_secs(10))]
    async fn test_jq_evaluation_timeout() {
        let jq_code = "def infinite_loop: infinite_loop; infinite_loop";
        let payload = serde_json::json!({});

        assert!(
            matches!(
                evaluate_jq(jq_code, &payload, std::time::Duration::from_secs(1)).await,
                Err(JqError::Timeout { .. })
            ),
            "Expected jq evaluation to timeout but it didn't"
        );
    }
}
