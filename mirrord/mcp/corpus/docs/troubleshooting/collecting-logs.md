---
title: Collecting Logs
date: 2026-09-27T00:00:00.000Z
description: Where each mirrord component writes its logs, how to raise the log level, and what to include in a bug report
tags:
  - oss
  - team
  - enterprise
---

A mirrord session runs several processes: the CLI, the layer loaded into your application, the internal proxy on your machine, the agent in the cluster, and the operator if you use mirrord for Teams. Each one logs on its own. When something misbehaves, the logs from the component that failed are what we need to help you.

## CLI and layer

The CLI and the layer read their log level from the `MIRRORD_LOG` environment variable and write to stderr. The value follows the `RUST_LOG` convention. Set `MIRRORD_PROGRESS_MODE=off` as well, so the progress spinner does not interleave with the log lines:

```bash
MIRRORD_LOG=mirrord=trace MIRRORD_PROGRESS_MODE=off mirrord exec -- <your command>
```

To write layer logs to files instead of mixing them into your application's stderr, set `MIRRORD_LAYER_LOG_PATH` to a directory. mirrord creates one file per process, named `mirrord-layer_<timestamp>_<process>_pid<pid>`:

```bash
MIRRORD_LOG=mirrord=trace MIRRORD_PROGRESS_MODE=off MIRRORD_LAYER_LOG_PATH=/tmp/mirrord-logs mirrord exec -- <your command>
```

{% hint style="info" %}
`MIRRORD_LAYER_LOG_PATH` requires mirrord `3.188.0` or later. On Windows, `mirrord exec` from `3.245.0` on also writes a crash record and memory dump to this directory. When it is not set, a Windows `mirrord exec` run logs to a per-session folder under `%TEMP%\mirrord`, removed after a clean exit and kept after a crash. Sessions started from an IDE do not create that folder: set `MIRRORD_LAYER_LOG_PATH` in the run configuration to get layer log files.
{% endhint %}

## Internal proxy

The internal proxy runs on your machine and relays traffic between the layer and the agent. It always writes to a file. By default the file lives in your temporary directory and is named `mirrord-intproxy-<timestamp>-<random>.log`.

Raise the level and pick a fixed location in your mirrord config:

```json
{
  "internal_proxy": {
    "log_level": "mirrord=trace",
    "log_destination": "/tmp/mirrord-intproxy.log"
  }
}
```

`log_level` defaults to `mirrord=info,warn`. When you run with `mirrord container`, the external proxy logs the same way through `external_proxy.log_level` and `external_proxy.log_destination`, with files named `mirrord-extproxy-<timestamp>-<random>.log`.

## Agent

The agent runs in the cluster as a pod labeled `app=mirrord` with a container named `mirrord-agent`. Without the operator, the pod is created in your kubeconfig's current namespace, or in `agent.namespace` if you set it. A targetless session ignores `agent.namespace` and uses `target.namespace` instead. With the operator, the pod is created in the operator's namespace (`mirrord` by default). Set the agent's log level in your mirrord config:

```json
{
  "agent": {
    "log_level": "mirrord=trace",
    "ttl": 60
  }
}
```

`ttl` keeps the agent pod around for that many seconds after the session ends (default `1`), so you have time to read its logs:

```bash
kubectl logs -n <agent namespace> -l app=mirrord -c mirrord-agent
```

If you run the agent as an ephemeral container (`agent.ephemeral: true`), it lives on the target pod itself, under a container named `mirrord-agent-<random suffix>`. Look the name up, then read its logs:

```bash
kubectl get pod -n <target namespace> <target pod> -o jsonpath='{.spec.ephemeralContainers[*].name}'
kubectl logs -n <target namespace> <target pod> -c mirrord-agent-<suffix>
```

## Operator

For mirrord for Teams, the operator logs cover session creation, licensing, and cluster-side features:

```bash
kubectl logs --namespace mirrord deployment/mirrord-operator
```

The level is `info` by default. Raise it with `operator.logLevel` in the Helm chart values, or with the `RUST_LOG` environment variable on the operator container:

```yaml
operator:
  logLevel: mirrord=debug,operator=debug
```

See [Monitoring](../managing-mirrord/monitoring.md) for JSON logging and shipping operator logs to your logging stack.

## IDEs

The VS Code extension and the JetBrains plugin start the same CLI, so everything above applies. Log level and file locations for the layer come from the run configuration's environment variables, and the proxy and agent settings come from the mirrord config file the session uses.

### VS Code

- Extension logs: open the **Output** panel and pick **mirrord** from the dropdown. Use **Developer: Set Log Level...** from the command palette to raise the level for that channel.
- Layer logs: add `MIRRORD_LOG` and `MIRRORD_LAYER_LOG_PATH` to the `env` block of your launch configuration.
- Extension version: the Extensions view lists it next to the mirrord entry.

### JetBrains IDEs

- Plugin logs: go to **Help → Show Log in Finder** (or **Show Log in Explorer** on Windows). The plugin writes to the IDE log under the `mirrord` category.
- Layer logs: add `MIRRORD_LOG` and `MIRRORD_LAYER_LOG_PATH` to the environment variables of your run configuration.
- Plugin version: **Settings → Plugins → Installed**, under mirrord.

## What to include in a bug report

When you [open an issue](https://github.com/metalbear-co/mirrord/issues/new?assignees=&labels=bug&projects=&template=bug_report.yml) or ask on [Slack](https://metalbear.com/slack), attach:

- The layer, internal proxy, and agent logs from a run that reproduces the problem, at `mirrord=trace`
- Your mirrord config, with secrets removed
- Output of `mirrord --version`, and of `mirrord operator status` if you use the operator
- The agent version. By default it matches the CLI version. If you override the agent image with `agent.image` or `MIRRORD_AGENT_IMAGE`, or run with the operator, which sets the image in its Helm values under `agent.image`, take the tag from the running agent instead. For a standalone agent: `kubectl get pod -n <agent namespace> -l app=mirrord -o jsonpath='{.items[*].spec.containers[*].image}'`. For an ephemeral agent, list the target pod's ephemeral containers and take the `mirrord-agent-` line: `kubectl get pod -n <target namespace> <target pod> -o jsonpath='{range .spec.ephemeralContainers[*]}{.name}{" "}{.image}{"\n"}{end}'`
- The extension or plugin version if you run from an IDE
- Your operating system and version, and the local process you ran (language, runtime, version)
- The steps you took and what you expected to happen

Trace logs record what your process did, including the command line arguments of processes it started. Read through them and redact anything sensitive before you share them in a public issue or channel.
