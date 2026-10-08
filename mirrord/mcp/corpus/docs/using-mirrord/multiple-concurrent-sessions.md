---
title: "mirrord up (multiple concurrent sessions)"
tags:
  - alpha
  - oss
  - team
  - enterprise
description: "How to use mirrord up"
date: 2026-04-014T16:25:00+04:00
lastmod: 2026-04-014T16:25:00+04:00
draft: false
menu:
  docs:
    parent: "using-mirrord"
weight: 140
toc: true
---
## Introduction

`mirrord up` allows creating and running multiple mirrord sessions based on configuration defined in a single file — think `docker compose` but for mirrord. This can be useful for cases when you need to debug multiple related microservices and would like to manage their lifecycle together.

Each service in the file is typically a *different* application with its own target, command, and configuration; this is for debugging several distinct applications together and managing their lifecycle as one unit, not for targeting multiple pods of the same application. For that, see [Targeting Pods by Label](targeting-pods-by-label.md).

## Getting started

The fastest way to get a valid `mirrord-up.yaml` is the interactive wizard:

```sh
mirrord up init
```

It prompts for common settings and walks you through one or more services, then writes the file (default: `./mirrord-up.yaml`). The generated file contains only the values you set; everything left at its default is omitted. See [`mirrord up init`](#mirrord-up-init) below for details.

To write the file by hand instead, start with:

```yaml
services:
  user-auth-service:
    run:
      command: ["python", "-m", "http.server"]

  stage-user-dashboard-app:
    target:
      path: pod/nginx
    run:
      command: ["node", "app.js"]
```

This file is the single source of configuration for all running sessions. Each entry in `services` defines a `mirrord` process that will run as part of the `mirrord up` session. You can leave out `target.path` (or the whole `target`) and `mirrord up` will infer it from the service id — see the `services.*.target` section below.

Now, in the same directory of the `mirrord-up.yaml` file, run

```sh
mirrord up
```

This will start all defined services, and they will run in parallel. The `mirrord up` session will be stopped once it's interrupted (`ctrl-c`) or one of the running mirrord sessions shuts down.

Services default to `split` mode, which steals incoming traffic matching an `http_filter`. When no filter is provided, mirrord generates one based on the session key: `baggage: .*mirrord-session={key}.*`.

If you'd rather have your local process take over a service completely, use `replace` mode — see [Service modes](#service-modes).

## Configuration

### Service modes

Service modes are set per service with `default_mode` in the config file, or for a whole run with the `--mode` flag.

The following config file sets `replace` mode for `user-auth-service`, while leaving `stage-user-dashboard-app` in the default `split` mode.

```yaml
services:
  user-auth-service:
    default_mode: replace
    run:
      command: ["python", "-m", "http.server"]

  stage-user-dashboard-app:
    target:
      path: pod/nginx
    run:
      command: ["node", "app.js"]
```

To override the mode for every service in a run, use `mirrord up --mode replace`.

#### `split`

If none is given, every service uses `split` mode by default. Your local process and the deployed service both keep serving traffic, and only requests matching the service's `http_filter` are stolen to your machine. When no filter is provided, mirrord generates one from the session key: `baggage: .*mirrord-session={key}.*`.

#### `replace`

Your local process takes over the service entirely. mirrord creates a copy of the target workload and scales the original down to zero for the duration of the session, so every request that would have reached the deployed service reaches your machine instead.

{% hint style="warning" %}
`replace` scales the deployed workload down to zero while your session runs, so *everyone* hitting that service reaches your local process — not just you. On a shared cluster, prefer `split`.

The original workload is restored when the session ends.
{% endhint %}

`replace` requires the targeted workload to be a deployment, statefulset, or replicaset.

Any `http_filter` set on a service in `replace` mode is ignored.

#### `mirror`

{% hint style="info" %}
This feature requires at least mirrord version 3.258.0.
{% endhint %}

Traffic is mirrored to your local process, and the deployed service runs uninterrupted. Only requests matching the service's `http_filter` are mirrored to your machine. When no filter is provided, mirrord generates one from the session key: `baggage: .*mirrord-session={key}.*`.

### Queue Splitting

`mirrord up` supports queue splitting automatically for every service in `split` and `mirror` mode. You don't need to add any special configuration.

Services in `replace` mode don't use queue splitting: the deployed workload is scaled down, so your local process consumes every message from its queues.

Before starting the session, set up queue splitting for the target and enable the relevant queue-splitting feature in the mirrord operator. Follow the [Queue Splitting guide](../sharing-the-cluster/queue-splitting.md) for the target's `MirrordSplitConfig` and broker-specific prerequisites.

Start the services with a session key, for example:

```sh
mirrord up --key checkout-debug
```

Messages intended for this session must contain `mirrord-session=checkout-debug`.
The marker is matched in broker-specific message metadata: Kafka headers, RabbitMQ headers, SQS message attributes, Google Cloud Pub/Sub attributes,
Azure Service Bus application properties, or Temporal headers. For Redis Pub/Sub and BullMQ, it is matched in the message payload.

Queue splitting is available in `mirrord up` for Kafka, Amazon SQS, RabbitMQ, Google Cloud Pub/Sub, Azure Service Bus, Redis Pub/Sub, Temporal, and BullMQ.

{% hint style="info" %}
RabbitMQ splitting in `mirrord up` requires an operator that supports it. Against an older operator the session still runs, with RabbitMQ splitting disabled and a warning printed for the affected service.
{% endhint %}

{% hint style="info" %}
Only messages containing the session key (`checkout-debug` in this case) are routed to your local session. All other messages continue to the deployed target.
{% endhint %}

### Context

`mirrord up` allows you to specify which Kubernetes context to run services in. You can do this either with the `--context` flag, or by setting it in `context` in the config file.

The following config file sets the `kind` context for all services via `common` field, and overrides it to `minikube` for `user-auth-service`.

```yaml
common:
  context: kind
services:
  user-auth-service:
    context: minikube
    run:
      command: ["python", "-m", "http.server"]

  stage-user-dashboard-app:
    target:
      path: pod/nginx
    run:
      command: ["node", "app.js"]
```

To override the context for every service in a run, use `mirrord up --context minikube`.

#### Context precedence

| common context | service context | `--context` | context used              |
|----------------|-----------------|-------------|---------------------------|
| any            | any             | set         | `--context`               |
| any            | set             | unset       | service context           |
| set            | unset           | unset       | common context            |
| unset          | unset           | unset       | default (current context) |

### Config file (`mirrord-up.yaml`)

#### `common`

Common configuration options, applied to all defined services. Currently the following options are supported:
- [`accept_invalid_certificates`](https://metalbear.com/mirrord/docs/config/options#root-accept_invalid_certificates)
- [`operator`](https://metalbear.com/mirrord/docs/config/options#root-operator)
- [`telemetry`](https://metalbear.com/mirrord/docs/config/options#root-telemetry)
- [`context`](https://metalbear.com/mirrord/docs/config/options#root-kube_context)

All fields map directly to their `mirrord.json` counterparts.

#### `services`

A map from service ids to a `ServiceConfig`. Each entry in this map defines and configures a mirrord process that will be run as part of the session.

##### `services.*.target`

Specifies the target of the session. Has 2 fields: `path` and `namespace`, which map directly to their `mirrord.json` counterparts.

When `path` is omitted, `mirrord up` infers it from the service id (the key in the `services` map) by searching the cluster for a deployment, statefulset, rollout, or pod with that name. If a match is found, it's used automatically, otherwise `mirrord up` prompts you to pick a namespace and workload, and offers to save the choice back to `mirrord-up.yaml` so future runs skip the prompt.

To run a service without a target (outgoing traffic only), set `target: none`.

Examples:

```yaml
# Specify both namespace and path
target:
  path: deployment/test-app
  namespace: test-namespace
```

```yaml
# Path only, will use default namespace
target:
  path: deployment/test-app
```

```yaml
# Path omitted: inferred from the service id, searching `test-namespace`
target:
  namespace: test-namespace
```

```yaml
# Targetless — outgoing traffic only
target: none
```

Omitting the `target` field entirely is equivalent to an empty mapping: the path is inferred from the service id in the default namespace.

##### `services.*.env`

Specifies the environment variable configuration for the given service. Maps directly (1:1) to [`feature.env`](https://metalbear.com/mirrord/docs/config/options#feature-env)

##### `services.*.default_mode`

Either `split` (the default), `replace` or `mirror`. See [Service modes](#service-modes) for what each one does and when to use it.

The `--mode` flag overrides this for every service being launched.

##### `services.*.http_filter`

Specifies the HTTP filtering configuration for the given service. Maps directly to [`feature.network.incoming.http_filter`](https://metalbear.com/mirrord/docs/config/options#feature-network-incoming)

Only applies in `split` and `mirror` modes. A service in `replace` mode receives all incoming traffic, so any filter set on it is ignored.

##### `services.*.ignore_ports`

List of ports that should be ignored in incoming traffic. Maps directly to [`feature.network.incoming.ignore_ports`](https://metalbear.com/mirrord/docs/config/options#feature-network-incoming)

##### `services.*.config_patch`

Some mirrord configuration options are not available in the `mirrord-up.yaml` format. With `config_patch` you're able to expand what's supported and use
regular `mirrord.json` options for a specific service. Prefer using the dedicated `mirrord-up.yaml` fields whenever possible, the `config_patch` is
an escape hatch while other settings are being ported over to `mirrord up`.

```yaml
config_patch:
  feature:
    split_queues:
      "*":
        queue_type: SQS
        jq_filter: '.Body | fromjson | .headers["x-meow-id"] == "{{ key }}"'
```

##### `services.*.run`

Specifies the command that should be run with mirrord. Has 2 fields:

- `command`: Array of strings containing the command to be run and its CLI arguments.
- `type`: can be either `exec` or `container`, defaults to `exec`. Specifies how mirrord should be run (i.e. with `mirrord exec` or `mirrord container`)

Examples:

```yaml
run:
  type: container
  command: ["docker", "run", "my-app"]
```

```yaml
run:
  # `type` defaults to `exec`, no need to specify explicitly
  command: ["node", "app.js"]
```

##### `services.*.context`
The name of the Kubernetes context to run this service in. See [Context](#context) for precedence rules when used with `common.context`.

The `--context` argument overrides this for every service being launched.

### Templating

The `mirrord-up.yaml` configuration file is rendered with [Tera](https://keats.github.io/tera/docs/) (Jinja2-style syntax) before it is parsed. This lets you populate configuration values dynamically from the session key and from your shell environment.

#### Available variables

- `{{ key }}` — The session key (specified via `--key` flag or defaults to OS username)

#### Available functions

- `{{ get_env(name="VAR") }}` — The value of the environment variable `VAR` from the environment `mirrord up` was started in. Rendering fails if the variable isn't set, unless you pass a fallback: `{{ get_env(name="VAR", default="fallback") }}`.

#### Examples

Use the session key in environment variable overrides:

```yaml
services:
  my-service:
    target:
      path: deployment/my-app
    env:
      override:
        SESSION_ID: "{{ key }}"
        DEBUG_TAG: "debug-{{ key }}"
    run:
      command: ["node", "app.js"]
```

Use the session key in commands:

```yaml
services:
  logger:
    run:
      command: ["python", "logger.py", "--session", "{{ key }}"]
```

When you run `mirrord up --key my-session`, the above examples will render as:

- `SESSION_ID: "my-session"`
- `DEBUG_TAG: "debug-my-session"`
- Command: `["python", "logger.py", "--session", "my-session"]`

Pull values from the environment instead of hardcoding them per developer:

```yaml
services:
  my-service:
    target:
      namespace: "{{ get_env(name='DEV_NAMESPACE', default='default') }}"
    env:
      override:
        API_TOKEN: "{{ get_env(name='API_TOKEN') }}"
    run:
      command: ["node", "app.js"]
```

Here the namespace falls back to `default` when `DEV_NAMESPACE` isn't set, while a missing `API_TOKEN` fails the run with a templating error rather than starting the session with an empty value.

## CLI args

### `-f`, `--config-file`

Allows specifying a different config file, e.g. `mirrord up -f mirrord-up-custom.yaml`

### `-m`, `--mode`

Runs every service in the given mode, ignoring the `default_mode` set in the config file. Either `split`, `replace` or `mirror` — see [Service modes](#service-modes). When omitted, each service uses its own `default_mode`.

### `--key`

Allows specifying a custom session key. When not supplied, the OS username is used.

### `--context`

Runs every service in the specified context, ignoring the `context` field(s) set in the config file. When omitted, each service uses the `context` in the config file. See [Context](#context) for precedence rules.

## `mirrord up init`

Interactive wizard that generates a skeleton `mirrord-up.yaml`. It does not query the cluster; workload inference and prompting happen later, when you run `mirrord up`.

```sh
mirrord up init [-o path/to/mirrord-up.yaml]
```

Flow:

1. **Common settings** — prompts for `operator`, `accept_invalid_certificates`, and `telemetry`. Only values you change from the default are written.
2. **Services** — loops one service at a time, prompting for name, mode, target, HTTP filter, ignore ports (with presets for Istio/Linkerd sidecars), env overrides, run type (`exec`/`container`), and the local command. For the target you choose to infer it from the service name (looked up when you run `mirrord up`), specify one explicitly, or run without a target. Choosing `replace` mode skips the HTTP filter prompt and drops the targetless option, since neither applies to it. Repeats until you answer "no" to *Add another service?*.
3. **Preview and save** — prints the generated YAML, asks whether to save, then for a filename (re-asking if you decline to overwrite an existing file).
