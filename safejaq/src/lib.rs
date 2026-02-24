#![cfg(unix)]

use std::{borrow::Cow, io::Read, ops::Deref, process::Stdio, time::Duration};

use jaq_core::{
    Ctx, RcIter,
    load::{Arena, File, Loader},
};
use jaq_json::Val;
use nix::{libc::rlim_t, sys::resource::Resource};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::Command,
};

/// Request to evaluate a JAQ filter against a payload.
#[derive(Deserialize, Serialize)]
pub struct EvaluationRequest<'a> {
    pub filter: Cow<'a, str>,
    pub payload: Cow<'a, serde_json::Value>,
}

/// Result of evaluating a JAQ filter against a payload.
pub type EvaluationResult = Result<bool, String>;

/// Allows for evaluating untrusted JAQ filters with configurable time
/// and memory limits. Works by re-execing the mirrord-agent
/// executable with special commandline flags and using rlimit on the
/// child process.
pub struct SafeJaq {
    time_limit: Duration,
    memory_limit: u64,
}

#[derive(Error, Debug)]
pub enum SafeJaqError {
    #[error("failed to use the evaluator command: {0}")]
    Command(#[from] std::io::Error),

    #[error(
        "filter evaluation exceeded either the time limit of {}ms or the memory limit of {} bytes",
        .0.as_millis(),
        .1,
    )]
    LimitExceeded(Duration, u64),

    #[error("failed to evaluate the filter: {0}")]
    Evaluation(String),
}

impl SafeJaq {
    /// Creates a new instance.
    ///
    /// # Params
    ///
    /// * `extraction_dir` - directory where the JAQ evaluator binary will be extracted
    /// * `time_limit` - time limit for evaluating a filter
    /// * `memory_limit` - memory limit for evaluating a filter
    pub fn new(time_limit: Duration, memory_limit: u64) -> Self {
        Self {
            time_limit,
            memory_limit,
        }
    }

    /// Evaluates the given JAQ filter against the given payload,
    /// respecting the configured time and memory limits.
    pub async fn evaluate(
        &self,
        filter: &str,
        payload: &serde_json::Value,
    ) -> Result<bool, SafeJaqError> {
        let mut child = Command::new(std::env::current_exe()?)
            .args([
                "jaq-eval",
                "-m",
                &self.memory_limit.to_string(),
                "-t",
                &self.time_limit.as_secs().to_string(),
            ])
            .stdout(Stdio::piped())
            .stdin(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .map_err(SafeJaqError::Command)?;

        let request = serde_json::to_string(&EvaluationRequest {
            filter: Cow::Borrowed(filter),
            payload: Cow::Borrowed(payload),
        })
        .expect("serializing simple struct to memory should not fail");

        // Send the evaluation request to the child process
        // and wait for it to finish.
        // Since the time limit passed to the child is counted in seconds,
        // we use our own timeout here.
        let result = tokio::time::timeout(self.time_limit, async {
            child
                .stdin
                .as_mut()
                .expect("was piped")
                .write_all(request.as_bytes())
                .await?;
            child.stdin.as_mut().expect("was piped").shutdown().await?;
            child.wait().await?;
            Ok::<_, std::io::Error>(())
        })
        .await;

        let Ok(Ok(())) = result else {
            // If the child process did not finish in time, or IO on
            // pipes failed, assume it's because the evaluation
            // exceeded the limits. To uncover any potential bugs,
            // wait for the child to finish and log its output, in the
            // background. The child may not always exit (if it
            // sleeps/does IO/whatever and doesn't exhaust the CPU
            // time limit), so we need an additional timeout on our
            // side. We do it in the background because it might take
            // over a second.
            tokio::spawn(async move {
                match tokio::time::timeout(Duration::from_secs(3), child.wait()).await {
                    Ok(Ok(status)) => {
                        let stderr = if let Some(mut stderr) = child.stderr {
                            let mut buf = vec![];
                            // This should always finish since the child has exited
                            Some(stderr.read_to_end(&mut buf).await.map(|_size| buf))
                        } else {
                            None
                        };
                        tracing::warn!(
                            status = %status,
                            ?stderr,
                            "JAQ evaluator command finished after exceeding limits",
                        );
                    }
                    Ok(Err(error)) => {
                        tracing::error!(
                            %error,
                            "Failed to collect output of JAQ evaluator command after exceeding limits",
                        );
                    }
                    Err(_elapsed) => {
                        tracing::error!(
                            "JAQ evaluator command does not want to exit, shutting it down forcefully."
                        );
                        if let Err(err) = child.kill().await {
                            tracing::warn!(?err, "failed to kill misbehaving jaq evaluator child");
                        }
                    }
                }
            });
            return Err(SafeJaqError::LimitExceeded(
                self.time_limit,
                self.memory_limit,
            ));
        };

        // The child process has already finished, so `wait_with_output` here should finish
        // instantly.
        let stdout = match child.wait_with_output().await {
            Ok(output) if output.status.success() => output.stdout,
            Ok(output) => {
                tracing::warn!(
                    status = %output.status,
                    stderr = %String::from_utf8_lossy(&output.stderr),
                    "JAQ evaluator command failed",
                );
                return Err(SafeJaqError::LimitExceeded(
                    self.time_limit,
                    self.memory_limit,
                ));
            }
            Err(error) => return Err(SafeJaqError::Command(error)),
        };

        match serde_json::from_slice::<EvaluationResult>(&stdout) {
            Ok(result) => result.map_err(SafeJaqError::Evaluation),
            Err(error) => Err(SafeJaqError::Command(std::io::Error::other(format!(
                "command printed malformed output: {error}"
            )))),
        }
    }
}

pub fn evaluator_main(memory_limit: u64, time_limit: u64) -> ! {
    set_limits(memory_limit, time_limit);

    let mut buf = Vec::new();
    std::io::stdin()
        .lock()
        .read_to_end(&mut buf)
        .expect("failed to read stdin");
    let request = serde_json::from_slice::<EvaluationRequest>(&buf)
        .expect("failed to parse EvaluationRequest");
    let result = evaluate(request);
    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer(&mut stdout, &result).expect("failed to write EvaluationResult");

    std::process::exit(0)
}
fn set_limits(memory_limit: rlim_t, time_limit: rlim_t) {
    // Set the total virtual memory limit
    let (soft_limit, _) =
        nix::sys::resource::getrlimit(Resource::RLIMIT_AS).expect("failed to get RLIMIT_AS");

    if memory_limit < soft_limit {
        nix::sys::resource::setrlimit(Resource::RLIMIT_AS, memory_limit, memory_limit)
            .expect("failed to set RLIMIT_AS");
    }

    let (soft_limit, _) =
        nix::sys::resource::getrlimit(Resource::RLIMIT_CPU).expect("failed to get RLIMIT_CPU");
    if time_limit < soft_limit {
        nix::sys::resource::setrlimit(Resource::RLIMIT_CPU, time_limit, time_limit)
            .expect("failed to set RLIMIT_CPU");
    }

    // Disable core dumps
    nix::sys::resource::setrlimit(Resource::RLIMIT_CORE, 0, 0).expect(
        "failed to set
    RLIMIT_CORE",
    );
}

fn evaluate(request: EvaluationRequest) -> Result<bool, String> {
    let program = File {
        code: request.filter.deref(),
        path: (),
    };
    let loader = Loader::new(jaq_std::defs().chain(jaq_json::defs()));
    let arena = Arena::default();
    let modules = loader
        .load(&arena, program)
        .map_err(|errors| format!("failed to parse the filter: {errors:?}"))?;

    let filter = jaq_core::Compiler::default().with_funs(jaq_std::funs().chain(jaq_json::funs()));

    let filter = filter
        .compile(modules)
        .map_err(|errors| format!("failed to compile the filter: {errors:?}"))?;

    let inputs = RcIter::new(core::iter::empty());
    let mut out = filter.run((
        Ctx::new([], &inputs),
        Val::from(request.payload.into_owned()),
    ));

    let found_match = out
        .find_map(|item| {
            if let Ok(Val::Bool(value)) = &item {
                Some(*value)
            } else {
                None
            }
        })
        .unwrap_or(false);
    Ok(found_match)
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case(
        "(.user_id // \"\") | test(\"^(liron|\\\\d+)$\")",
        serde_json::json!({
            "user_id": "liron",
        }),
        Some(true)
    )]
    #[case(
        "(.user_id // \"\") | test(\"^(liron|\\\\d+)$\")",
        serde_json::json!({
            "user_id": "definitely not liron",
        }),
        Some(false)
    )]
    #[case(
        "i am really not a valid filter",
        serde_json::json!({}),
        None,
    )]
    #[test]
    fn test_evaluate_inner(
        #[case] filter: &str,
        #[case] payload: serde_json::Value,
        #[case] expected: Option<bool>,
    ) {
        let result = super::evaluate(EvaluationRequest {
            filter: filter.into(),
            payload: Cow::Owned(payload),
        });

        match (result, expected) {
            (Ok(true), Some(true)) => {}
            (Ok(false), Some(false)) => {}
            (Err(..), None) => {}
            (result, Some(value)) => panic!("unexpected result: {result:?}, expected {value}"),
            (result, None) => panic!("unexpected result: {result:?}, expected an error"),
        }
    }
}
