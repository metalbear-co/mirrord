//! `mirrord pitm` - Process In The Middle.
//!
//! Windows-only subcommand that handles scenarios where the IDE cannot
//! start the injection target process suspended. It proxies the process
//! creation through mirrord, which starts it suspended and does not
//! resume with user code execution until the mirrord layer is injected.
//! It also extracts the child's mirrord environment from a dedicated
//! side-channel env var and applies it to the child process directly,
//! so the mirrord-specific variables never touch `pitm`'s own process
//! environment or any other unrelated process in the tree.
//!
//! `mirrord attach` solves the same problem when the IDE allows for the
//! process to be suspended before user code can run and allows us to get
//! the PID. For run configurations that only expose "launch this
//! command", like JetBrains IDEs, the target process is already
//! executing user code by the time the plugin can observe the PID, so
//! any work that happened in those first hundreds of milliseconds
//! escapes mirrord's hooks entirely.
//!
//! `pitm` takes ownership of the full child lifecycle: it is invoked as
//! a process-in-the-middle in place of the user's binary, creates the
//! real target with `CREATE_SUSPENDED`, injects the layer DLL, waits
//! for the layer to signal ready, then resumes execution. From the
//! first instruction of user code, mirrord hooks are already in place.
//!
//! The plugin passes the child process's environment through a single
//! `MIRRORD_CHILD_ENV` variable, whose value is the base64 encoding of
//! `{"set": {VAR: VAL, ...}, "unset": [VAR, ...]}`. This keeps the
//! mirrord env vars isolated to the child; the `pitm` process itself
//! runs with a clean environment.
//!
//! Everything else (intproxy startup, target resolution, config
//! rendering) has already been done by the plugin via `mirrord ext`
//! before `pitm` is invoked. `pitm` does no k8s, no agent handshake,
//! no config loading; it is the thinnest possible shim around
//! [`LayerManagedProcess::execute`].

use std::collections::HashMap;

use base64::{Engine, engine::general_purpose::STANDARD};
use mirrord_layer_lib::process::windows::execution::{LayerManagedProcess, MIRRORD_LAYER_FILE_ENV};
use mirrord_progress::NullProgress;
use serde::Deserialize;

use crate::{CliResult, config::PitmArgs, error::CliError, extract::extract_library};

/// Name of the env var the plugin uses to ferry the child environment
/// into `pitm` without polluting `pitm`'s own process environment.
const MIRRORD_CHILD_ENV: &str = "MIRRORD_CHILD_ENV";

/// Path to the real `java.exe` to launch when this binary is invoked as a
/// Java launcher shim. See [`run_as_java_launcher`].
const MIRRORD_PITM_REAL_JAVA: &str = "MIRRORD_PITM_REAL_JAVA";

/// Decoded payload of [`MIRRORD_CHILD_ENV`].
///
/// Mirrors the shape produced by `mirrord ext`: a set of variables to
/// add/overwrite on the child, plus a list of variables to remove from
/// the inherited environment.
#[derive(Deserialize, Debug, Default)]
struct ChildEnv {
    #[serde(default)]
    set: HashMap<String, String>,
    #[serde(default)]
    unset: Vec<String>,
}

/// Detects when mirrord is started as `java.exe` and dispatches directly to
/// [`pitm_command`].
///
/// The IntelliJ IDEA plugin constructs a fake JDK whose `bin/java.exe` is a
/// copy of this binary, then points a Java run configuration's SDK at that
/// fake home. The `bin/java.exe` suffix is required: it is how IntelliJ's
/// `JavaParameters` is turned into a command line, so the shim must live at
/// exactly that relative path inside the fake JDK. When IntelliJ launches the
/// run, it executes
/// `<fakeJdk>/bin/java.exe <jvm args> <Main class> <program args>`, which is
/// actually this binary.
///
/// We detect that case by inspecting `argv[0]`. If the basename is `java.exe`,
/// we read the real `java.exe` path from [`MIRRORD_PITM_REAL_JAVA`], build
/// [`PitmArgs`] manually (real java as the exe, everything else verbatim), and
/// call [`pitm_command`]. Clap never runs, so the JVM args (which look nothing
/// like mirrord subcommands) are never misparsed.
///
/// Returns `None` when not in java-launcher mode so the caller falls through
/// to normal clap parsing.
pub(crate) fn run_as_java_launcher() -> Option<miette::Result<()>> {
    let argv0 = std::env::args_os().next()?;
    let basename = std::path::Path::new(&argv0)
        .file_name()?
        .to_str()?
        .to_ascii_lowercase();

    if basename != "java.exe" {
        return None;
    }

    let real_java = match std::env::var(MIRRORD_PITM_REAL_JAVA) {
        Ok(v) => v,
        Err(_) => {
            return Some(Err(miette::miette!(
                "mirrord was invoked as java.exe but {MIRRORD_PITM_REAL_JAVA} is not set"
            )));
        }
    };

    // Forward everything after argv[0] verbatim to the real java process.
    let mut command = vec![real_java];
    command.extend(std::env::args().skip(1));

    let args = PitmArgs { command };
    Some(pitm_command(args).map_err(Into::into))
}

/// `pitm` runs silently on the happy path: the plugin expects the child
/// process's stdout/stderr to pass through verbatim, and any extra
/// chatter from `mirrord` itself would leak into the IDE's run console.
/// Errors still surface through the usual [`CliError`] path, so a real
/// failure is never hidden.
pub(crate) fn pitm_command(args: PitmArgs) -> CliResult<()> {
    let (exe, extra_args) = args.command.split_first().ok_or(CliError::PitmMissingExe)?;

    let encoded = std::env::var(MIRRORD_CHILD_ENV).map_err(|_| CliError::PitmMissingChildEnv)?;
    let decoded_bytes = STANDARD
        .decode(encoded.as_bytes())
        .map_err(|e| CliError::PitmInvalidChildEnv(format!("base64 decode: {e}")))?;
    let child_env: ChildEnv = serde_json::from_slice(&decoded_bytes)
        .map_err(|e| CliError::PitmInvalidChildEnv(format!("json parse: {e}")))?;

    let mut env_vars = build_child_environment(child_env);

    // Extract the layer DLL to the temp dir and point the child at it.
    // `LayerManagedProcess::execute` reads `MIRRORD_LAYER_FILE` from the
    // supplied env_vars map, so we embed it there rather than mutating
    // the current process's environment.
    let lib_path = extract_library(None, &NullProgress, false)?;
    env_vars.insert(
        MIRRORD_LAYER_FILE_ENV.to_owned(),
        lib_path.to_string_lossy().into_owned(),
    );

    let command_line = build_command_line(exe, extra_args);

    let managed = LayerManagedProcess::execute(
        Some(exe.to_owned()),
        command_line,
        None,
        env_vars,
        None::<NullProgress>,
    )
    .map_err(|e| CliError::PitmExecuteFailed(exe.to_owned(), e.to_string()))?;

    let exit_code = managed
        .wait_until_exit()
        .map_err(|e| CliError::PitmExecuteFailed(exe.to_owned(), e.to_string()))?;

    std::process::exit(exit_code as i32);
}

/// Compose the final environment block for the child process.
///
/// Starts from the current process environment, strips
/// [`MIRRORD_CHILD_ENV`] (the child has no reason to see the envelope
/// it was delivered in), applies the plugin's requested `set` overrides
/// last so they win over inherited values, and finally removes any
/// variables the plugin asked to `unset`. The `unset` pass runs after
/// `set` so a variable present in both lists is unset -- matching the
/// principle of least surprise for a "remove these" directive.
fn build_child_environment(child_env: ChildEnv) -> HashMap<String, String> {
    let mut env: HashMap<String, String> = std::env::vars().collect();
    env.remove(MIRRORD_CHILD_ENV);
    for (k, v) in child_env.set {
        env.insert(k, v);
    }
    for k in child_env.unset {
        env.remove(&k);
    }
    env
}

/// Build a Windows command line string from an executable path plus
/// arguments, applying CRT-compatible quoting.
///
/// `CreateProcessW` does no parsing when `lpApplicationName` is set,
/// but the child process's own CRT (and anything using
/// `CommandLineToArgvW`) will still tokenise `lpCommandLine` to
/// populate `argv`. Convention is therefore that `argv[0]` is the
/// module name repeated as the first token; some applications rely on
/// this and some do not, but it costs nothing to be correct.
fn build_command_line(exe: &str, args: &[String]) -> String {
    let mut out = String::with_capacity(exe.len() + 2);
    push_quoted(&mut out, exe);
    for arg in args {
        out.push(' ');
        push_quoted(&mut out, arg);
    }
    out
}

/// Append `arg` to `out`, quoted according to the rules documented for
/// `CommandLineToArgvW` / the MSVC CRT parser.
///
/// Runs of backslashes followed by a literal double quote have to be
/// doubled, and the quote itself escaped, so that the parser on the
/// other side reconstructs the original string exactly. A bare arg
/// with no whitespace or quotes is passed through verbatim; this keeps
/// the resulting command line readable in the common case.
fn push_quoted(out: &mut String, arg: &str) {
    let needs_quoting = arg.is_empty() || arg.chars().any(|c| matches!(c, ' ' | '\t' | '"' | '\n'));
    if !needs_quoting {
        out.push_str(arg);
        return;
    }

    out.push('"');
    let mut iter = arg.chars().peekable();
    while let Some(c) = iter.next() {
        match c {
            '\\' => {
                let mut backslashes: usize = 1;
                while iter.peek() == Some(&'\\') {
                    iter.next();
                    backslashes += 1;
                }
                match iter.peek() {
                    None => {
                        for _ in 0..backslashes * 2 {
                            out.push('\\');
                        }
                    }
                    Some(&'"') => {
                        for _ in 0..backslashes * 2 + 1 {
                            out.push('\\');
                        }
                        out.push('"');
                        iter.next();
                    }
                    Some(_) => {
                        for _ in 0..backslashes {
                            out.push('\\');
                        }
                    }
                }
            }
            '"' => {
                out.push('\\');
                out.push('"');
            }
            other => out.push(other),
        }
    }
    out.push('"');
}
