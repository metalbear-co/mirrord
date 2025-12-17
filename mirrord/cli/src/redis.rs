use std::process::Stdio;

use tokio::process::Command;

use crate::{CliResult, config::RedisArgs};

const DEFAULT_REDIS_IMAGE: &str = "redis:7-alpine";

pub async fn redis_command(args: RedisArgs) -> CliResult<()> {
    let RedisArgs {
        source,
        source_namespace,
        local_port: _,
        container_name,
        image,
    } = args;

    let namespace = source_namespace.unwrap_or_else(|| "default".to_string());
    let branch_name =
        container_name.unwrap_or_else(|| format!("redis-branch-{}", std::process::id()));
    let image = image.unwrap_or_else(|| DEFAULT_REDIS_IMAGE.to_string());

    println!("Creating Redis branch pod in cluster...");

    // Create the pod
    let pod_manifest = format!(
        r#"{{
  "apiVersion": "v1",
  "kind": "Pod",
  "metadata": {{
    "name": "{}",
    "namespace": "{}",
    "labels": {{
      "app": "redis-branch",
      "mirrord.metalbear.co/redis-branch": "true"
    }}
  }},
  "spec": {{
    "containers": [{{
      "name": "redis",
      "image": "{}",
      "ports": [{{ "containerPort": 6379 }}]
    }}]
  }}
}}"#,
        branch_name, namespace, image
    );

    let output = Command::new("kubectl")
        .args(["apply", "-f", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| crate::CliError::RedisError(format!("Failed to run kubectl: {}", e)))?;

    let mut child = output;
    if let Some(mut stdin) = child.stdin.take() {
        use tokio::io::AsyncWriteExt;
        stdin
            .write_all(pod_manifest.as_bytes())
            .await
            .map_err(|e| crate::CliError::RedisError(format!("Failed to write manifest: {}", e)))?;
    }

    let output = child
        .wait_with_output()
        .await
        .map_err(|e| crate::CliError::RedisError(format!("kubectl failed: {}", e)))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(crate::CliError::RedisError(format!(
            "Failed to create pod: {}",
            stderr
        )));
    }

    println!("Waiting for pod to be ready...");

    // Wait for pod to be ready
    let wait_output = Command::new("kubectl")
        .args([
            "wait",
            "--for=condition=Ready",
            &format!("pod/{}", branch_name),
            "-n",
            &namespace,
            "--timeout=60s",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|e| crate::CliError::RedisError(format!("kubectl wait failed: {}", e)))?;

    if !wait_output.status.success() {
        let stderr = String::from_utf8_lossy(&wait_output.stderr);
        return Err(crate::CliError::RedisError(format!(
            "Pod not ready: {}",
            stderr
        )));
    }

    // Copy data if source provided, otherwise empty branch
    if let Some(ref src) = source {
        println!("Copying data from {}/{}...", namespace, src);
        copy_data_in_cluster(src, &branch_name, &namespace).await?;
    }

    Ok(())
}

/// Source can be deploy/name, deployment/name, or pod-name
async fn copy_data_in_cluster(source: &str, target_pod: &str, namespace: &str) -> CliResult<()> {
    // Get all keys from source Redis
    let keys_output = Command::new("kubectl")
        .args([
            "exec",
            "-n",
            namespace,
            source,
            "--",
            "redis-cli",
            "KEYS",
            "*",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|e| crate::CliError::RedisError(format!("kubectl exec failed: {}", e)))?;

    if !keys_output.status.success() {
        let stderr = String::from_utf8_lossy(&keys_output.stderr);
        return Err(crate::CliError::RedisError(format!(
            "Failed to get keys: {}",
            stderr
        )));
    }

    let keys_str = String::from_utf8_lossy(&keys_output.stdout);
    let keys: Vec<&str> = keys_str.lines().filter(|k| !k.is_empty()).collect();

    if keys.is_empty() {
        println!("No keys in source Redis");
        return Ok(());
    }

    println!("Found {} keys, copying...", keys.len());

    let mut copied = 0;
    for key in &keys {
        // Get value from source
        let get_output = Command::new("kubectl")
            .args([
                "exec",
                "-n",
                namespace,
                source,
                "--",
                "redis-cli",
                "GET",
                key,
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await;

        let value = match get_output {
            Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).to_string(),
            _ => continue,
        };

        // Set value in target
        let set_output = Command::new("kubectl")
            .args([
                "exec",
                "-n",
                namespace,
                target_pod,
                "--",
                "redis-cli",
                "SET",
                key,
                value.trim(),
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await;

        if let Ok(result) = set_output {
            if result.status.success() {
                copied += 1;
            }
        }
    }

    println!("Copied {}/{} keys", copied, keys.len());
    Ok(())
}
