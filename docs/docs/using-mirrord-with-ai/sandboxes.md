---
title: Run AI Sandbox Code Against Staging
description: Prepare the setup that all AI sandbox platforms share to run the code that an AI agent writes in a sandbox against the real services in your shared staging cluster with mirrord.
draft: false
toc: true
tags: ["enterprise"]
---

# Run AI Sandbox Code Against Staging

Many AI agents run their code in a remote sandbox, such as an E2B or Modal sandbox. The sandbox isolates the code from your machines and your cluster. It does not give the code the services, queues, and databases of your real environment. The agent then can only check that the code compiles and that the unit tests pass.

mirrord adds that connection. The sandbox runs the code, and mirrord in the sandbox connects the code to a target in your staging cluster. The agent can then test a change against the real APIs, queues, and databases. The process steals only the requests that carry its own header, so other developers and agents continue to use staging.

This page holds the setup that all sandbox platforms share: the cluster access, the mirrord configuration, and the check script. Each platform page gives a Python script that creates a sandbox, starts a service under mirrord, sends one request through staging, checks the answer, and stops mirrord:

- [Test E2B Sandbox Code Against Staging](./e2b.md)
- [Test Modal Sandbox Code Against Staging](./modal.md)

## How it works

The agent, or a script that the agent calls, creates a sandbox from an image that contains mirrord. The sandbox gets a kubeconfig and a CI API key as secrets. mirrord starts the service in the sandbox and connects to the mirrord Operator through the Kubernetes API. The Operator starts a session on the staging target. The target continues to serve all requests, except the requests whose header carries the session key of the agent. mirrord sends those requests to the service in the sandbox.

![How mirrord connects a sandbox to the staging cluster](../.gitbook/assets/ai-sandbox-flow.svg)

The scripts use `mirrord ci start`, not `mirrord up`. `mirrord ci start` starts the service in the background and returns, so one shell script can start the service, send the request, and stop the session with `mirrord ci stop`.

## What you need

- The mirrord Operator in the cluster, on the Enterprise plan.
- A CI API key and a Kubernetes identity for your agents. See [Using mirrord with Cloud Agents](./cloud-agents.md) for how to create them. Sessions with a CI API key are machine sessions and do not take up seats.
- A Kubernetes API server that the sandbox can reach from the internet. The sandbox platform must also let the sandbox open outbound connections to it.
- A kubeconfig for the agent identity that does not need an auth plugin, for example a kubeconfig with a service account token. A sandbox image usually does not contain plugins such as `gke-gcloud-auth-plugin` or `aws`.
- A staging service with an HTTP endpoint that the sandbox can reach, for example through an ingress. The ingress must forward the `x-mirrord-agent` header to the service.
- An account and an API key for the sandbox platform, and the Python SDK of that platform on the machine that runs the script.

Bind the agent identity to the `mirrord-operator-ci` ClusterRole. Also give it `get` on the target deployment and `list` on pods in the target namespace. See [Security](../managing-mirrord/security.md) for the built-in roles. For example, this Role and RoleBinding give these permissions to the service account `mirrord-agent`:

```yaml
apiVersion: rbac.authorization.k8s.io/v1
kind: Role
metadata:
  name: mirrord-sandbox-target
  namespace: staging
rules:
  - apiGroups: ["apps"]
    resources: ["deployments"]
    resourceNames: ["orders-api"]
    verbs: ["get"]
  - apiGroups: [""]
    resources: ["pods"]
    verbs: ["list"]
---
apiVersion: rbac.authorization.k8s.io/v1
kind: RoleBinding
metadata:
  name: mirrord-sandbox-target
  namespace: staging
roleRef:
  apiGroup: rbac.authorization.k8s.io
  kind: Role
  name: mirrord-sandbox-target
subjects:
  - kind: ServiceAccount
    name: mirrord-agent
    namespace: staging
```

## Prepare the files for the sandbox

All platform scripts put the same two files in the sandbox: a mirrord configuration and a shell script that runs the check.

### The mirrord configuration

Save this file as `mirrord.json` next to the Python script. Change the target to your staging service.

```json
{
  "target": {
    "path": "deployment/orders-api",
    "namespace": "staging"
  },
  "operator": true,
  "feature": {
    "network": {
      "incoming": {
        "mode": "steal",
        "http_filter": {
          "header_filter": "^x-mirrord-agent: {{ key }}$"
        }
      }
    }
  },
  "ci": {
    "output_dir": "/tmp/mirrord-ci"
  }
}
```

The `{{ key }}` template gets the session key that the script gives with `--key`. The header filter is a regular expression, and the `^` and `$` anchors stop one key from matching a longer key. For example, without the anchors the filter for `agent-1` also matches `agent-10`. Use keys that contain only letters, numbers, and dashes. `ci.output_dir` is the directory where mirrord writes the output of the service.

### The check script

Save this file as `check.sh` next to the Python script. It runs in the sandbox, and it takes the command that starts the service as its arguments. The check passes when a request with the header gets the answer of the service in the sandbox, and a request without the header gets the answer of staging.

{% hint style="warning" %}
`mirrord ci start` returns some seconds before the steal is active. The script sends requests with the header until the service in the sandbox answers. Do not remove that loop.
{% endhint %}

```bash
#!/bin/bash
# Runs in the sandbox: bash /tmp/check.sh <command that starts the service>
# Needs KUBECONFIG_B64, MIRRORD_CI_API_KEY, AGENT_KEY, APP_PORT, STAGING_URL, and EXPECTED.
set -euo pipefail

# Without a terminal, the default progress mode prints nothing.
export MIRRORD_PROGRESS_MODE=simple

mkdir -p "$HOME/.kube"
echo "$KUBECONFIG_B64" | base64 -d > "$HOME/.kube/config"
chmod 600 "$HOME/.kube/config"

trap 'mirrord ci stop || true' EXIT
mirrord ci start -f /tmp/mirrord.json --key "$AGENT_KEY" -- "$@"

# Wait until the service listens on its port.
for i in $(seq 1 60); do
  if (exec 3<>"/dev/tcp/127.0.0.1/$APP_PORT") 2>/dev/null; then break; fi
  if [ "$i" -eq 60 ]; then echo "The service did not start." >&2; exit 1; fi
  sleep 2
done

# The steal starts some seconds after mirrord ci start returns.
for i in $(seq 1 30); do
  answer=$(curl -s --connect-timeout 5 --max-time 10 \
    -H "x-mirrord-agent: $AGENT_KEY" "$STAGING_URL" || true)
  if [[ "$answer" == *"$EXPECTED"* ]]; then break; fi
  if [ "$i" -eq 30 ]; then
    echo "The answer with the header is not from the sandbox: $answer" >&2; exit 1
  fi
  sleep 2
done
echo "With the header: $answer"

# A request without the header must get a successful answer of staging.
if ! answer=$(curl -sf --connect-timeout 5 --max-time 10 "$STAGING_URL"); then
  echo "Staging did not send a successful answer to the request without the header." >&2; exit 1
fi
echo "Without the header: $answer"
if [[ "$answer" == *"$EXPECTED"* ]]; then
  echo "A request without the header got the answer of the sandbox." >&2; exit 1
fi
echo "The sandbox got the request with the header, and staging got the request without it."
```

### The values for the Python scripts

Each Python script reads these environment variables on your machine:

- `SANDBOX_KUBECONFIG`: the path of the kubeconfig for the agent identity.
- `MIRRORD_CI_API_KEY`: the CI API key from `mirrord ci api-key`.
- `STAGING_URL`: the staging URL to send the request to, for example `https://orders.staging.example.com/health`.

Each script also has these constants. Change them to match your service:

- `MIRRORD_VERSION`: the mirrord release to install in the image. Use the same version for all sandboxes.
- `APP_REPO`: the repository of the service. In an agent sandbox, the code is usually there already. Then remove the `git clone` step.
- `APP_CMD`: the command that starts the service, for example `python3 app.py`.
- `APP_PORT`: the port that the service listens on. It must be the port of the target container.
- `EXPECTED`: a text that only the answer of the changed code contains, for example `"version": "sandbox"`. The answer of staging must not contain it.

Then run the script for your platform. See [Test E2B Sandbox Code Against Staging](./e2b.md) or [Test Modal Sandbox Code Against Staging](./modal.md).

## Behavior and limits

These limits apply on all platforms. Each platform page adds the limits of that platform.

- **The platform must allow the layer.** mirrord loads its layer into the service with `LD_PRELOAD`. A sandbox with its own Linux kernel, such as an E2B sandbox or a Modal sandbox with `runtime="vm"`, behaves like a Linux VM. A gVisor sandbox, such as a Modal sandbox with the gVisor runtime, implements the Linux system calls itself, and a Python service does not work there. See [Test Modal Sandbox Code Against Staging](./modal.md).
- **The image needs glibc.** The mirrord Linux binary needs glibc. Images based on Alpine or another musl distribution do not work, also with `gcompat`. Use a Debian or Ubuntu image.
- **mirrord runs on Linux x86_64 and aarch64 only.** The install script stops on other architectures.
- **The sandbox must reach the Kubernetes API server.** mirrord connects to the Operator through the Kubernetes API, so the sandbox needs no other path into the cluster. If your API server accepts only some source addresses, add the egress addresses of the sandbox platform.
- **Each sandbox needs its own session key.** Sandboxes with different keys can target the same service at the same time. Sandboxes with the same key get each other's requests.
- **The session ends with mirrord.** The session ends when `mirrord ci stop` runs or when the platform stops the sandbox. Set the sandbox timeout longer than the tests.
- **Nothing from the sandbox is deployed.** The staging target continues to serve all requests without the header.
- **Queues and databases need their own filters.** For queues and databases, use queue splitting and DB branching with the session key. See [Using mirrord with Cloud Agents](./cloud-agents.md#4-keep-agent-sessions-apart).

## Troubleshooting

These problems can occur on all platforms. Each platform page adds the problems of that platform.

| Problem | Cause and fix |
|---|---|
| mirrord prints nothing, and the command does not end. | The sandbox command has no terminal. Set `MIRRORD_PROGRESS_MODE=simple`. |
| `mirrord: not found`, or the binary does not start, on an Alpine image. | The image uses musl. Use an image with glibc, such as Debian. |
| `` `MIRRORD_CI_API_KEY` is required `` or the key does not decode. | The variable is not set, or it holds a placeholder, such as the value of a platform secret. Give the real key as an environment variable. |
| `... is forbidden: User "system:serviceaccount:..." cannot ...` | The agent identity does not have the permission. Bind it to `mirrord-operator-ci`, and give it `get` on the target deployment and `list` on pods. |
| A timeout when mirrord connects to the Kubernetes API. | The platform or the cluster blocks the connection. Allow the API server in the egress settings of the sandbox, and allow the platform in the authorized networks of the cluster. |
| `executable "gke-gcloud-auth-plugin" not found` or a similar error. | The kubeconfig uses an auth plugin that is not in the image. Use a kubeconfig with a service account token. |
| The staging service, not the sandbox, answers the request. | The steal is not active yet, or the header does not match. Send requests again for some seconds, and check the key and the header name. Also check that the ingress forwards the header. |

For logs, see [Collecting logs](../troubleshooting/collecting-logs.md). `mirrord ci start` writes the output of the service to `ci.output_dir`.

## FAQ

### Does each sandbox take a mirrord seat?

No. Sessions that use a CI API key are machine sessions. They count towards the concurrent machine sessions of your [plan](https://metalbear.com/mirrord/pricing/).

### Can many agents use the same staging service at the same time?

Yes. Give each sandbox a different session key. Each session then steals only the requests with its own header, and the staging service serves all other requests.

### Why use `mirrord ci start` and not `mirrord exec` in a sandbox?

`mirrord ci start` runs the service in the background and returns. The same script can then send requests and run tests, and `mirrord ci stop` ends the session.

### Do I need to expose the cluster to the internet?

The sandbox needs to reach the Kubernetes API server. The check script on this page also sends requests from the sandbox to `STAGING_URL`. The sandbox needs a route to that endpoint, for example through an ingress. You do not need to expose the other staging services or the mirrord Operator. Limit access to the API server to the egress addresses of the sandbox platform where the platform publishes them.

## See also

- [Test E2B Sandbox Code Against Staging](./e2b.md)
- [Test Modal Sandbox Code Against Staging](./modal.md)
- [Using mirrord with Cloud Agents](./cloud-agents.md)
- [CI/CD Integration](../use-cases/mirrord-for-ci.md)
- [Filtering Incoming Traffic](../using-mirrord/incoming-traffic/filter-incoming-traffic.md)
- [Queue Splitting](../sharing-the-cluster/queue-splitting.md)
- [DB Branching](../sharing-the-cluster/db-branching.md)
- [Security](../managing-mirrord/security.md)
