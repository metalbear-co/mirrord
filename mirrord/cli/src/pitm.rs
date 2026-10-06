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

use std::{collections::HashMap, path::Path};

use base64::{Engine, engine::general_purpose::STANDARD};
use mirrord_layer_lib::process::windows::{
    command_line::build_command_line,
    environment::WindowsEnv,
    execution::{LayerManagedProcess, MIRRORD_LAYER_FILE_ENV},
    injection::{InjectionMethod, MIRRORD_INJECTION_METHOD_ENV},
};
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
/// There is no `--injection-method` flag on this path, so the method comes from
/// `MIRRORD_INJECTION_METHOD` as described in [`child_environment`].
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

    let args = PitmArgs {
        command,
        injection_method: None,
    };
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

    // A build-specific file name prevents a newer CLI from reusing a stale layer left by an
    // older `pitm` invocation.
    let lib_path = extract_library(None, &NullProgress, true)?;
    let child = child_environment(
        WindowsEnv::inherited(),
        child_env,
        args.injection_method,
        &lib_path,
    )?;

    let command_line = build_command_line(exe, extra_args);

    let managed = LayerManagedProcess::execute(
        Some(exe.to_owned()),
        command_line,
        None,
        child.environment,
        child.injection_method,
        // Bind the child JVM's lifetime to this pitm process via a kill-on-close job,
        // so IntelliJ/Gradle abruptly stopping the run can't orphan it (an orphaned,
        // still-connected layer keeps the agent alive → "dirty iptables" next session).
        true,
        None::<NullProgress>,
    )
    .map_err(|e| CliError::PitmExecuteFailed(exe.to_owned(), e.to_string()))?;

    let exit_code = managed
        .wait_until_exit()
        .map_err(|e| CliError::PitmExecuteFailed(exe.to_owned(), e.to_string()))?;

    std::process::exit(exit_code as i32);
}

/// What `pitm` launches its child with.
#[derive(Debug)]
struct ChildLaunch {
    environment: WindowsEnv,
    injection_method: InjectionMethod,
}

/// Composes the child's environment and selects how it is injected.
///
/// The environment starts from `inherited` (this process's environment) and strips
/// [`MIRRORD_CHILD_ENV`] (the child has no reason to see the envelope it was delivered in). The
/// plugin's `set` overrides are applied next, so they win over inherited values, and then the
/// variables the plugin asked to `unset` are removed, so a variable in both lists is unset --
/// matching the principle of least surprise for a "remove these" directive. Last,
/// `MIRRORD_LAYER_FILE` points the child at the extracted layer. Names follow Windows rules, so an
/// override of `PATH` replaces an inherited `Path` instead of sitting next to it.
///
/// The injection method, highest precedence first:
///
/// 1. `flag`, the `--injection-method` on the `pitm` command line;
/// 2. `MIRRORD_INJECTION_METHOD` in the composed environment, which holds the plugin's `set` value
///    when it has one and the value `pitm` inherited otherwise;
/// 3. [`InjectionMethod::default`].
///
/// An explicit flag is the most deliberate choice, so it wins. Without one, the value the child
/// would see anyway is honoured, which is how the plugin or the user's environment picks the
/// method for launches whose command line they do not control, such as the Java launcher shim.
/// An unparsable value is an error rather than a silent fallback to the default. The launch
/// writes the chosen method's canonical name into the child's environment.
fn child_environment(
    mut inherited: WindowsEnv,
    child_env: ChildEnv,
    flag: Option<InjectionMethod>,
    lib_path: &Path,
) -> CliResult<ChildLaunch> {
    inherited.remove(MIRRORD_CHILD_ENV);
    for (name, value) in child_env.set {
        inherited.set(&name, value);
    }
    for name in child_env.unset {
        inherited.remove(&name);
    }
    inherited.set(
        MIRRORD_LAYER_FILE_ENV,
        lib_path.to_string_lossy().into_owned(),
    );

    let injection_method = match flag {
        Some(method) => method,
        None => inherited
            .get(MIRRORD_INJECTION_METHOD_ENV)
            .map(InjectionMethod::parse)
            .transpose()
            .map_err(CliError::PitmInvalidInjectionMethod)?
            .unwrap_or_default(),
    };

    Ok(ChildLaunch {
        environment: inherited,
        injection_method,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const LAYER: &str = r"C:\layers\mirrord_layer.dll";

    fn launch(
        inherited: &[(&str, &str)],
        set: &[(&str, &str)],
        unset: &[&str],
        flag: Option<InjectionMethod>,
    ) -> CliResult<ChildLaunch> {
        let owned = |entries: &[(&str, &str)]| {
            entries
                .iter()
                .map(|&(name, value)| (name.to_owned(), value.to_owned()))
                .collect::<Vec<_>>()
        };
        child_environment(
            WindowsEnv::from_ordered_entries(owned(inherited)),
            ChildEnv {
                set: owned(set).into_iter().collect(),
                unset: unset.iter().map(|&name| name.to_owned()).collect(),
            },
            flag,
            Path::new(LAYER),
        )
    }

    /// The plugin's overrides replace inherited names in any casing, its unsets win over its own
    /// overrides, the envelope is gone, and the child is pointed at the extracted layer.
    #[test]
    fn the_plugin_shapes_the_childs_environment() {
        let child = launch(
            &[
                ("Path", r"C:\inherited"),
                ("Temp", r"C:\scratch"),
                (MIRRORD_CHILD_ENV, "envelope"),
                ("mirrord_layer_file", "stale.dll"),
            ],
            &[("PATH", r"C:\override"), ("GONE", "x")],
            &["temp", "GONE"],
            None,
        )
        .unwrap();
        assert_eq!(
            child.environment.iter().collect::<Vec<_>>(),
            [(MIRRORD_LAYER_FILE_ENV, LAYER), ("PATH", r"C:\override")]
        );
    }

    #[test]
    fn the_flag_wins_over_the_childs_environment() {
        let child = launch(
            &[(MIRRORD_INJECTION_METHOD_ENV, "apc")],
            &[],
            &[],
            Some(InjectionMethod::Iat),
        )
        .unwrap();
        assert_eq!(child.injection_method, InjectionMethod::Iat);
    }

    /// Without a flag, the value the child would see selects the method: the plugin's over the
    /// inherited one, under any casing of the name.
    #[test]
    fn without_a_flag_the_childs_environment_selects_the_method() {
        let child = launch(
            &[("mirrord_injection_method", "apc")],
            &[(MIRRORD_INJECTION_METHOD_ENV, "IAT")],
            &[],
            None,
        )
        .unwrap();
        assert_eq!(child.injection_method, InjectionMethod::Iat);

        let child = launch(&[("mirrord_injection_method", "apc")], &[], &[], None).unwrap();
        assert_eq!(child.injection_method, InjectionMethod::Apc);

        let child = launch(&[], &[], &[], None).unwrap();
        assert_eq!(child.injection_method, InjectionMethod::default());
    }

    #[test]
    fn an_invalid_method_in_the_childs_environment_is_an_error() {
        assert!(matches!(
            launch(&[(MIRRORD_INJECTION_METHOD_ENV, "bogus")], &[], &[], None),
            Err(CliError::PitmInvalidInjectionMethod(_))
        ));
    }
}
