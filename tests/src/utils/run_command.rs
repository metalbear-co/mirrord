#![allow(dead_code)]
use std::{collections::HashMap, process::Stdio};

use tempfile::tempdir;
use tokio::process::Command;

use super::process::TestProcess;

/// Run `mirrord exec` without specifying a target, to run in targetless mode.
pub async fn run_exec_targetless(
    process_cmd: Vec<&str>,
    namespace: Option<&str>,
    args: Option<Vec<&str>>,
    env: Option<Vec<(&str, &str)>>,
) -> TestProcess {
    run_exec(process_cmd, None, namespace, args, env).await
}

/// See [`run_exec`].
pub async fn run_exec_with_target(
    process_cmd: Vec<&str>,
    target: &str,
    namespace: Option<&str>,
    args: Option<Vec<&str>>,
    env: Option<Vec<(&str, &str)>>,
) -> TestProcess {
    run_exec(process_cmd, Some(target), namespace, args, env).await
}

pub async fn run_mirrord(args: Vec<&str>, env: HashMap<&str, &str>) -> TestProcess {
    let path = match option_env!("MIRRORD_TESTS_USE_BINARY") {
        None => env!("CARGO_BIN_FILE_MIRRORD"),
        Some(binary_path) => binary_path,
    };
    let temp_dir = tempdir().unwrap();

    let server = Command::new(path)
        .args(&args)
        .envs(env)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    println!(
        "executed mirrord with args {args:?} pid {}",
        server.id().unwrap()
    );
    // We need to hold temp dir until the process is finished
    TestProcess::from_child(server, Some(temp_dir))
}

/// Run `mirrord exec` with the given cmd, optional target (`None` for targetless), namespace,
/// mirrord args, and env vars.
pub async fn run_exec(
    process_cmd: Vec<&str>,
    target: Option<&str>,
    namespace: Option<&str>,
    args: Option<Vec<&str>>,
    env: Option<Vec<(&str, &str)>>,
) -> TestProcess {
    let mut mirrord_args = vec!["exec", "-c"];
    if let Some(target) = target {
        mirrord_args.extend(["--target", target].into_iter());
    }
    if let Some(namespace) = namespace {
        mirrord_args.extend(["--target-namespace", namespace].into_iter());
    }

    if let Some(args) = args {
        mirrord_args.extend(args.into_iter());
    }
    mirrord_args.push("--");
    let args: Vec<&str> = mirrord_args
        .into_iter()
        .chain(process_cmd.into_iter())
        .collect();
    let agent_image_env = "MIRRORD_AGENT_IMAGE";
    let agent_image_from_devs_env = std::env::var(agent_image_env);
    // used by the CI, to load the image locally:
    // docker build -t test . -f mirrord/agent/Dockerfile
    // minikube load image test:latest
    let mut base_env = HashMap::new();
    base_env.insert(
        agent_image_env,
        // Let devs running the test specify an agent image per env var.
        agent_image_from_devs_env.as_deref().unwrap_or("test"),
    );
    base_env.insert("MIRRORD_CHECK_VERSION", "false");
    base_env.insert("MIRRORD_AGENT_RUST_LOG", "warn,mirrord=debug");
    base_env.insert("MIRRORD_AGENT_COMMUNICATION_TIMEOUT", "180");
    // We're using k8s portforwarding for sending traffic to test services.
    // The packets arrive to loopback interface.
    base_env.insert("MIRRORD_AGENT_NETWORK_INTERFACE", "lo");
    base_env.insert("RUST_LOG", "warn,mirrord=debug");

    if let Some(env) = env {
        for (key, value) in env {
            base_env.insert(key, value);
        }
    }

    run_mirrord(args, base_env).await
}

/// Runs `mirrord ls` command.
pub async fn run_ls(namespace: &str) -> TestProcess {
    let mut mirrord_args = vec!["ls"];
    mirrord_args.extend(vec!["--namespace", namespace]);

    let mut env = HashMap::new();
    let use_operator = cfg!(feature = "operator").to_string();
    env.insert("MIRRORD_OPERATOR_ENABLE", use_operator.as_str());

    run_mirrord(mirrord_args, env).await
}

/// Runs `mirrord verify-config [--ide] "/path/config.json"`.
///
/// ## Attention
///
/// The `verify-config` tests are here to guarantee that your changes do not break backwards
/// compatability, so you should not modify them, only add new tests (unless breakage is
/// wanted/required).
pub async fn run_verify_config(args: Option<Vec<&str>>) -> TestProcess {
    let mut mirrord_args = vec!["verify-config"];
    if let Some(args) = args {
        mirrord_args.extend(args);
    }

    run_mirrord(mirrord_args, Default::default()).await
}
