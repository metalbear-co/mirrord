---
title: Serverless Workloads
description: >-
  Run a service locally while it takes over the network and environment of a
  workload running outside Kubernetes, such as an Amazon ECS task.
tags:
  - experimental
  - enterprise
---

# Serverless Workloads

{% hint style="warning" %}
Serverless workloads are **experimental**. They're available on request, and behavior, configuration and resource names may change without notice. Don't rely on them for anything beyond development environments. To try them, install a [supported mirrord CLI](#required-versions).
{% endhint %}

Some services don't run in Kubernetes, but on platforms such as Amazon ECS and Fargate. mirrord calls these **serverless workloads**. You run such a service **locally** while it takes over the network and environment of the deployed one. There's no image to build or push, and nothing to redeploy.

**When is this useful?**

1.  **Services outside Kubernetes**

    Part of your system runs on ECS and part on Kubernetes. You already use mirrord for the Kubernetes services, and want the same workflow for the others.
2.  **Resources only reachable from the workload**

    Your service talks to databases, queues or internal APIs that are only reachable from its network. Running it locally through mirrord sends its outgoing connections through the serverless workload, so the local process reaches them too.
3.  **Shared staging environments**

    Several developers work on the same service in one environment. Each developer steals only the requests that carry their own session key, so regular traffic and other developers are unaffected.

***

## Components

A serverless workload session has three parts:

* **Your local mirrord**, started with `mirrord exec` as usual.
* **The mirrord remote bootstrap**, a library loaded into the workload's existing application container. It starts a mirrord agent beside your application and registers it, under a service and environment name, with sessions-manager.
* **Sessions-manager**, which pairs your local mirrord with a registered workload and relays mirrord traffic between them. Both sides only make outbound connections to sessions-manager, so neither needs a network path to the other.

Where sessions-manager runs is the [deployment mode](#deployment-modes).

## Deployment Modes

| Mode | Sessions-manager runs | Authentication | Status |
| --- | --- | --- | --- |
| **Operator-hosted** | Inside the [mirrord Operator](../../managing-mirrord/operator.md) on your Kubernetes cluster, enabled with a Helm value. Both sides reach it through your cluster's API server. | Kubernetes identities and RBAC: developers use their kubeconfig, workloads a platform identity mapped into the cluster. | Experimental |
| **MetalBear Cloud** | Hosted by MetalBear, so there's nothing for you to install or operate, and no Kubernetes cluster or Operator is needed. | Workloads use a MetalBear API key; developers sign in with `mirrord login`. | Experimental |

Developers target a serverless workload the same way, with a `serverless/` target, in every mode. The mode changes how sessions-manager is installed, and how each side connects and authenticates to it.

**Which mode should I use?** If you already run the mirrord Operator, and your workloads can reach your cluster's API server, use the Operator-hosted mode: access is managed with the same Kubernetes RBAC as the rest of mirrord, and there are no secrets to distribute. Use MetalBear Cloud when you don't run the Operator, or when your workloads can't reach the API server.

## Supported Workloads

| Platform | Operator-hosted | MetalBear Cloud |
| --- | --- | --- |
| [Amazon ECS](ecs.md) | [Supported](ecs-operator-hosted.md) | [Supported](ecs-metalbear-cloud.md) |

## Setting Up

Setup is split by what it touches, so you only redo the parts that change:

1. **Set up your deployment mode, once.** See [Operator-Hosted Sessions-Manager](operator-hosted.md) or [MetalBear Cloud Sessions-Manager](metalbear-cloud.md).
2. **Add the remote bootstrap to each workload.** For ECS, see [Amazon ECS](ecs.md).
3. **Connect each workload to your deployment mode.** For ECS, see [Connecting ECS to the Operator](ecs-operator-hosted.md) or [Connecting ECS to MetalBear Cloud](ecs-metalbear-cloud.md).

### Required versions

* **Operator:** version **3.223.0 or later**, installed from the `metalbear/mirrord-operator` Helm chart. Follow the [Operator installation guide](../../managing-mirrord/operator.md), then [enable sessions-manager](operator-hosted.md#enable-sessions-manager).
* **Remote bootstrap:** `ghcr.io/metalbear-co/mirrord-remote-bootstrap:3.229.0` or later, used in the [ECS task definition](ecs.md#add-the-remote-bootstrap). Bootstrap tags follow Operator release versions, not mirrord CLI versions. Pin a release tag rather than `latest`.
* **CLI:** mirrord **3.282.0 or later**, a regular release, installed as described in [Installing the mirrord CLI](../../installing-mirrord/cli.md).

Earlier CLIs and bootstraps don't connect to the MetalBear Cloud sessions-manager by default. To use them with MetalBear Cloud, set `MIRRORD_SESSIONS_MANAGER_URL=https://sm-aws-use1.app.metalbear.com/sm` both in the developer's environment and on the workload, or upgrade.

### API key

In the MetalBear Cloud mode, workloads authenticate with a MetalBear API key for serverless workloads, which you can generate at [app.metalbear.com](https://app.metalbear.com). The setup guides refer to it as `<API_KEY>`. It begins with `metalbear_key_`. Developers don't need it: they sign in with `mirrord login`. The Operator-hosted mode needs nothing from us beyond your regular Operator license.

### Application container requirements

The remote bootstrap and the mirrord agent it starts run inside your application container, against the C library that container's image ships. The image must be:

* **Linux on x86-64 or ARM64.**
* **Based on glibc 2.30 or later**, such as Debian 11, Ubuntu 20.04, Amazon Linux 2023 or Red Hat Enterprise Linux 9 and later, or distroless images built on them. Amazon Linux 2, Red Hat Enterprise Linux 8 and Debian 10 ship older versions and aren't supported.
* **Not musl-based**, such as Alpine. The bootstrap is linked against glibc and can't be loaded there.

On an older glibc, the application container stops at startup, with an error that version `GLIBC_2.30` isn't found, required by `libmirrord_remote_bootstrap.so`. Remove the bootstrap from the task, or move the image to a supported base.

To check an image that has a shell, run `docker run --rm --entrypoint ldd <YOUR_APPLICATION_IMAGE> --version`. The first line shows the glibc version, or `musl` for a musl-based image.

***

## Using mirrord with a Serverless Workload

Install a [supported mirrord CLI](#required-versions), then create a `mirrord.json` in your project. This one is for the Operator-hosted mode; for MetalBear Cloud, see [With MetalBear Cloud](#with-metalbear-cloud) below.

```json
{
  "operator": true,
  "target": {
    "path": "serverless/<YOUR_SERVICE_NAME>",
    "namespace": "<YOUR_ENVIRONMENT>"
  },
  "key": "<YOUR_SESSION_KEY>",
  "feature": {
    "env": true,
    "network": {
      "outgoing": true,
      "incoming": {
        "mode": "steal",
        "http_filter": {
          "header_filter": "^baggage: .*mirrord-session={{ key }}.*$"
        }
      }
    }
  }
}
```

* [`operator: true`](https://metalbear.com/mirrord/docs/config/options#root-operator) together with a `serverless/` target makes mirrord reach sessions-manager through the Operator, using your kubeconfig's current context. This is the Operator-hosted mode: you need nothing else beyond a kubeconfig for the cluster that runs the Operator. Without it, mirrord connects to MetalBear Cloud instead; see [Which sessions-manager mirrord connects through](#which-sessions-manager-mirrord-connects-through).
* [`target.path`](https://metalbear.com/mirrord/docs/config/options#target-path) is `serverless/` followed by the service name, and [`target.namespace`](https://metalbear.com/mirrord/docs/config/options#target-namespace) is the environment the service runs in. These are the names your platform team gave the workload when setting it up, not Kubernetes resources or namespaces. Ask them for the values.
* [`key`](https://metalbear.com/mirrord/docs/config/options#root-key) identifies your session. It's substituted into `{{ key }}` in the HTTP filter, so only requests carrying `baggage: mirrord-session=<YOUR_SESSION_KEY>` reach your local process. Regular traffic and other developers' requests keep going to the serverless workload. See [Filtering Incoming Traffic](../incoming-traffic/filter-incoming-traffic.md) for other filters.
* `feature.network.outgoing` sends your local process's outgoing connections through the serverless workload, so they reach whatever the workload can reach.
* `feature.env` gives your local process the serverless workload's environment variables.

### With MetalBear Cloud

In the MetalBear Cloud mode, mirrord connects to sessions-manager directly rather than through the Operator, and authenticates with your MetalBear account instead of your kubeconfig. Sign in once, to the organization that runs the workloads:

```bash
mirrord login
```

Then leave `operator` out of `mirrord.json` (or set it to `false`), and keep the rest as above. See [MetalBear Cloud Sessions-Manager](metalbear-cloud.md#developer-setup) for details.

### Which sessions-manager mirrord connects through

mirrord picks the sessions-manager from your configuration, in this order:

1. [`operator: true`](https://metalbear.com/mirrord/docs/config/options#root-operator): the sessions-manager hosted by the Operator, reached through your kubeconfig's current context. `sessions_manager.url` and `MIRRORD_SESSIONS_MANAGER_URL` are then ignored.
2. A URL in [`sessions_manager.url`](https://metalbear.com/mirrord/docs/config/options#sessions_manager-url), or in the `MIRRORD_SESSIONS_MANAGER_URL` environment variable, which overrides it: the sessions-manager at that URL, including any path prefix it's served under. A value that isn't a valid URL is an error; mirrord doesn't fall back to MetalBear Cloud.
3. Otherwise, the MetalBear Cloud sessions-manager, at `https://sm-aws-use1.app.metalbear.com/sm`.

When the session starts, mirrord prints which sessions-manager it connects through, and why, for example `Connecting through the MetalBear Cloud sessions-manager at https://sm-aws-use1.app.metalbear.com/sm`. If that isn't the one your deployment mode uses, check `operator` in `mirrord.json`, and whether your shell or IDE run configuration sets `MIRRORD_SESSIONS_MANAGER_URL`.

The workload picks its sessions-manager the same way, from its own variables: `MIRRORD_OPERATOR_API_URL` selects the Operator, `MIRRORD_SESSIONS_MANAGER_URL` a sessions-manager at that URL, and with neither set, it registers with MetalBear Cloud. Your deployment mode's connection guide lists which to set.

### Running your service

Run your service:

```bash
mirrord exec -f mirrord.json -- <YOUR_COMMANDLINE>
```

To send a request to your local process, add the header to it, for example with `curl -H "baggage: mirrord-session=<YOUR_SESSION_KEY>" ...`. While browsing, the [mirrord browser extension](../incoming-traffic/debug-from-browser.md) can inject the header for you.

For every option, see the [configuration reference](https://metalbear.com/mirrord/docs/config/options).

***

## How It Works

1. **The workload registers.** When the workload starts, the remote bootstrap connects to sessions-manager and registers the workload under its service and environment names.
2. **You start mirrord.** `mirrord exec` connects to sessions-manager and asks for a session with the service and environment in your `mirrord.json`.
3. **Sessions-manager pairs them.** It assigns both sides to the same session and gives each a single-use credential for it.
4. **Both sides open the session's data plane.** Sessions-manager relays mirrord traffic between them. From here, mirrord behaves as it does with a Kubernetes target: incoming requests matching your filter go to your local process, and its outgoing traffic leaves from the serverless workload.

In the Operator-hosted mode, every connection goes through your cluster's API server to the Operator; see [Operator-Hosted Sessions-Manager](operator-hosted.md#how-it-works). In the MetalBear Cloud mode, both sides connect to the MetalBear-hosted sessions-manager over HTTPS: the workload authenticates with your API key, and you with the credentials `mirrord login` stored; see [MetalBear Cloud Sessions-Manager](metalbear-cloud.md#how-it-works).

***

## Limitations

* **One replica per session.** Your session is served by one registered replica of the service, such as one ECS task. If the service runs several, only requests that land on that replica can be stolen or mirrored. For testing, scale the service to one replica or route your test traffic to a specific one.
* **Operator-hosted mode:**
  * **Development traffic only.** Session traffic flows through your cluster's API server. That's fine for development, but don't use a mirrord session for load testing.
  * **Operator restarts end sessions.** Restarting or upgrading the Operator ends active sessions. The workload registers again on its own; rerun `mirrord exec`.
  * **Long-lived connections.** Any proxy or firewall between the workload and the API server must not buffer responses and needs an idle timeout of at least 300 seconds.
* **MetalBear Cloud mode:**
  * **Development traffic only.** Session traffic is relayed through the MetalBear-hosted sessions-manager. Don't use a mirrord session for load testing.
  * **Long-lived connections.** Any proxy or firewall between either side and sessions-manager must not buffer responses and needs an idle timeout of at least 300 seconds.
