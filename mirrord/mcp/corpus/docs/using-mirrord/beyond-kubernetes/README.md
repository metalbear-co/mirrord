---
title: Beyond Kubernetes
description: >-
  Run a service locally while it takes over the network and environment of a
  workload running outside Kubernetes, such as an Amazon ECS task.
tags:
  - alpha
  - team
  - enterprise
---

# Beyond Kubernetes

{% hint style="warning" %}
Support for workloads outside Kubernetes is in **alpha**: behavior, configuration and resource names may change without notice. Don't rely on them for anything beyond development environments. The MetalBear Cloud mode is available on every plan, including Free, while it's in alpha; the Operator-hosted mode requires the Enterprise plan. To try them, install a [supported mirrord CLI](#required-versions).
{% endhint %}

Some services don't run in Kubernetes, but on platforms such as Amazon ECS, Fargate and Cloudflare Containers. In this section, they're called **workloads outside Kubernetes**. The CLI and the dashboard call them serverless workloads, and developers target them with a `serverless/` target. You run such a service **locally** while it takes over the network and environment of the deployed one. There's no image to build or push, and nothing to redeploy.

**When is this useful?**

1.  **Services outside Kubernetes**

    Part of your system runs on ECS and part on Kubernetes. You already use mirrord for the Kubernetes services, and want the same workflow for the others.
2.  **Resources only reachable from the workload**

    Your service talks to databases, queues or internal APIs that are only reachable from its network. Running it locally through mirrord sends its outgoing connections through the workload, so the local process reaches them too.
3.  **Shared staging environments**

    Several developers work on the same service in one environment. Each developer steals only the requests that carry their own session key, so regular traffic and other developers are unaffected.

***

## Components

A session with a workload outside Kubernetes has three parts:

* **Your local mirrord**, started with [`mirrord up`](../multiple-concurrent-sessions.md).
* **The mirrord remote bootstrap**, a library loaded into the workload's existing application container. It starts a mirrord agent beside your application and registers it, under a service and environment name, with sessions-manager.
* **Sessions-manager**, which pairs your local mirrord with a registered workload and relays mirrord traffic between them. Both sides only make outbound connections to sessions-manager, so neither needs a network path to the other.

Where sessions-manager runs is the [deployment mode](#deployment-modes).

## Deployment Modes

| Mode | Sessions-manager runs | Authentication | Plans | Status |
| --- | --- | --- | --- | --- |
| **MetalBear Cloud** | Hosted by MetalBear, so there's nothing for you to install or operate, and no Kubernetes cluster or Operator is needed. | Workloads use a MetalBear API key; developers sign in with `mirrord login`. | All plans, including Free, while in alpha | Alpha |
| **Operator-hosted** | Inside the [mirrord Operator](../../managing-mirrord/operator.md) on your Kubernetes cluster, enabled with a Helm value. Both sides reach it through your cluster's API server. | Kubernetes identities and RBAC: developers use their kubeconfig, workloads a platform identity mapped into the cluster. | Enterprise | Alpha |

Developers target a workload outside Kubernetes the same way, with a `serverless/` target, in every mode. The mode changes how sessions-manager is installed, and how each side connects and authenticates to it.

Start with MetalBear Cloud. On the Enterprise plan, if you already run the Operator and your workloads can reach your cluster's API server, you can use the Operator-hosted mode instead: access is then managed with Kubernetes RBAC, with no API key to distribute.

## Supported Workloads

| Platform | MetalBear Cloud | Operator-hosted |
| --- | --- | --- |
| [Amazon ECS](ecs.md) | [Supported](ecs-metalbear-cloud.md) | [Supported](ecs-operator-hosted.md) |
| [Cloudflare Containers](cloudflare-containers.md) | [Supported](cloudflare-containers.md) | Not covered |

## Setting Up

Setup is split by what it touches, so you only redo the parts that change:

1. **Set up your deployment mode, once.** See [Operator-Hosted Sessions-Manager](operator-hosted.md) or [MetalBear Cloud Sessions-Manager](metalbear-cloud.md).
2. **Add the remote bootstrap to each workload.** For ECS, see [Amazon ECS](ecs.md). For Cloudflare Containers, see [Cloudflare Containers](cloudflare-containers.md), which also covers the next step.
3. **Connect each workload to your deployment mode.** For ECS, see [Connecting ECS to the Operator](ecs-operator-hosted.md) or [Connecting ECS to MetalBear Cloud](ecs-metalbear-cloud.md).

### Required versions

* **Operator:** version **3.223.0 or later**, installed from the `metalbear/mirrord-operator` Helm chart. Follow the [Operator installation guide](../../getting-started/install-the-operator.md#custom-install-with-helm), then [enable sessions-manager](operator-hosted.md#enable-sessions-manager).
* **Remote bootstrap:** `ghcr.io/metalbear-co/mirrord-remote-bootstrap:3.229.0` or later, used in the [ECS task definition](ecs.md#add-the-remote-bootstrap). Bootstrap tags follow Operator release versions, not mirrord CLI versions. Pin a release tag rather than `latest`.
* **CLI:** mirrord **3.288.0 or later**, a regular release, installed as described in [Install mirrord](../../getting-started/install-mirrord.md).

Bootstraps earlier than 3.229.0 don't connect to the MetalBear Cloud sessions-manager by default. To use one with MetalBear Cloud, set `MIRRORD_SESSIONS_MANAGER_URL=https://sm-aws-use1.app.metalbear.com/sm` on the workload, or upgrade.

### API key

In the MetalBear Cloud mode, workloads authenticate with a MetalBear serverless agent API key, which you can generate at [app.metalbear.com](https://app.metalbear.com). The setup guides refer to it as `<API_KEY>`. It begins with `metalbear_key_`. Developers don't need it: they sign in with `mirrord login`. The Operator-hosted mode needs nothing from us beyond your Enterprise Operator license.

### Application container requirements

The remote bootstrap and the mirrord agent it starts run inside your application container, against the C library that container's image ships. The image must be:

* **Linux on x86-64 or ARM64.**
* **Based on glibc 2.30 or later**, such as Debian 11, Ubuntu 20.04, Amazon Linux 2023 or Red Hat Enterprise Linux 9 and later, or distroless images built on them. Amazon Linux 2, Red Hat Enterprise Linux 8 and Debian 10 ship older versions and aren't supported.
* **Not musl-based**, such as Alpine. The bootstrap is linked against glibc and can't be loaded there.

On an older glibc, the application container stops at startup, with an error that version `GLIBC_2.30` isn't found, required by `libmirrord_remote_bootstrap.so`. Remove the bootstrap from the task, or move the image to a supported base.

To check an image that has a shell, run `docker run --rm --entrypoint ldd <YOUR_APPLICATION_IMAGE> --version`. The first line shows the glibc version, or `musl` for a musl-based image.

***

## Using mirrord with a Workload Outside Kubernetes

Install a [supported mirrord CLI](#required-versions), then create a `mirrord-up.yaml` in your project. This one is for the Operator-hosted mode; for MetalBear Cloud, see [With MetalBear Cloud](#with-metalbear-cloud) below.

```yaml
common:
  operator: true
services:
  <YOUR_SERVICE_NAME>:
    target:
      path: serverless/<YOUR_SERVICE_NAME>
      namespace: <YOUR_ENVIRONMENT>
    run:
      command: ["<YOUR_COMMAND>", "<ARG>"]
```

* [`common.operator: true`](../multiple-concurrent-sessions.md#common) together with a `serverless/` target makes mirrord reach sessions-manager through the Operator, using the service's kube [context](../multiple-concurrent-sessions.md#context), which falls back to your kubeconfig's current context. This is the Operator-hosted mode: you need nothing else beyond a kubeconfig for the cluster that runs the Operator. For MetalBear Cloud, set it to `false` instead; see [Which sessions-manager mirrord connects through](#which-sessions-manager-mirrord-connects-through).
* [`target.path`](../multiple-concurrent-sessions.md#services) is `serverless/` followed by the service name, and `target.namespace` is the environment the service runs in. These are the names your platform team gave the workload when setting it up, not Kubernetes resources or namespaces. Ask them for the values. When `namespace` isn't set, `default` is used.
* `run.command` is the command that starts your service locally.

`mirrord up` fills in the rest. The service runs in [`split` mode](../multiple-concurrent-sessions.md#split): only requests carrying `baggage: mirrord-session=<YOUR_SESSION_KEY>` reach your local process, and regular traffic and other developers' requests keep going to the workload outside Kubernetes. Your local process gets the workload's environment variables, reads files from the workload's application container, and resolves DNS names the way the workload does. Its outgoing connections leave through the workload, so they reach whatever it can reach. File access, environment variables, and DNS are configured as for a Kubernetes target, with [`feature.fs`](https://metalbear.com/mirrord/docs/config/options#feature-fs), [`feature.env`](https://metalbear.com/mirrord/docs/config/options#feature-env), and [`feature.network.dns`](https://metalbear.com/mirrord/docs/config/options#feature-network-dns) in the service's [`config_patch`](../multiple-concurrent-sessions.md#services). To change the filter, set [`http_filter`](../multiple-concurrent-sessions.md#services) on the service.

Services outside Kubernetes run in `split` or `mirror` mode only, without queue splitting or DB branching. You can list them in the same `mirrord-up.yaml` as services that target Kubernetes workloads. See [Serverless targets](../multiple-concurrent-sessions.md#serverless-targets) for the details.

### With MetalBear Cloud

In the MetalBear Cloud mode, mirrord connects to sessions-manager directly rather than through the Operator, and authenticates with your MetalBear account instead of your kubeconfig. Sign in once, to the organization that runs the workloads:

```bash
mirrord login
```

Then set `common.operator: false` in `mirrord-up.yaml`, and keep the rest as above. See [MetalBear Cloud Sessions-Manager](metalbear-cloud.md#developer-setup) for details.

### Which sessions-manager mirrord connects through

mirrord picks the sessions-manager from your configuration, in this order:

1. [`common.operator: true`](../multiple-concurrent-sessions.md#common), or `operator: true` in the global config `~/.mirrord/mirrord.json` when `common.operator` isn't set: the sessions-manager hosted by the Operator, reached through the service's kube [context](../multiple-concurrent-sessions.md#context). `sessions_manager.url` and `MIRRORD_SESSIONS_MANAGER_URL` are then ignored.
2. A URL in [`sessions_manager.url`](https://metalbear.com/mirrord/docs/config/options#sessions_manager-url), set through the service's [`config_patch`](../multiple-concurrent-sessions.md#services), or in the `MIRRORD_SESSIONS_MANAGER_URL` environment variable. When both are set, the `config_patch` URL is used, since `mirrord up` applies the patch after reading the environment: the sessions-manager at that URL, including any path prefix it's served under. A value that isn't a valid URL is an error; mirrord doesn't fall back to MetalBear Cloud.
3. Otherwise, the MetalBear Cloud sessions-manager, at `https://sm-aws-use1.app.metalbear.com/sm`.

When the session starts, mirrord prints which sessions-manager it connects through, and why, for example `connecting through the MetalBear Cloud sessions-manager at https://sm-aws-use1.app.metalbear.com/sm`. If that isn't the one your deployment mode uses, check `common.operator` in `mirrord-up.yaml` and `operator` in `~/.mirrord/mirrord.json`, where mirrord sets it to `true` after a successful Operator session, and whether your shell or IDE run configuration sets `MIRRORD_SESSIONS_MANAGER_URL`.

The workload picks its sessions-manager the same way, from its own variables: `MIRRORD_OPERATOR_API_URL` selects the Operator, `MIRRORD_SESSIONS_MANAGER_URL` a sessions-manager at that URL, and with neither set, it registers with MetalBear Cloud. Your deployment mode's connection guide lists which to set.

### Running your service

In the directory of `mirrord-up.yaml`, run:

```bash
mirrord up --key <YOUR_SESSION_KEY>
```

Without `--key`, your OS username is the session key. To send a request to your local process, add the header to it, for example with `curl -H "baggage: mirrord-session=<YOUR_SESSION_KEY>" ...`. While browsing, the [mirrord browser extension](../incoming-traffic/debug-from-browser.md) can inject the header for you.

For every option, see [mirrord up](../multiple-concurrent-sessions.md#config-file-mirrord-up.yaml).

### From your IDE

The [VS Code](../../installing-mirrord/vscode.md) and [IntelliJ](../../installing-mirrord/intellij.md) extensions don't run `mirrord up`. They run your service with a `mirrord.json`, so to debug a service outside Kubernetes from your IDE, use this one as the extension's [active config](../../installing-mirrord/vscode.md#active-config):

```json
{
  "operator": true,
  "target": {
    "path": "serverless/<YOUR_SERVICE_NAME>",
    "namespace": "<YOUR_ENVIRONMENT>"
  },
  "key": "<YOUR_SESSION_KEY>",
  "feature": {
    "network": {
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

It's the same session `mirrord up` starts in `split` mode: only requests carrying `baggage: mirrord-session=<YOUR_SESSION_KEY>` reach your local process. For MetalBear Cloud, set `"operator": false`. The [limitations](#limitations) apply the same way: setting `feature.split_queues`, `feature.kafka_branch`, `feature.db_branches` or `feature.copy_target` makes mirrord refuse to start the session.

***

## How It Works

1. **The workload registers.** When the workload starts, the remote bootstrap connects to sessions-manager and registers the workload under its service and environment names.
2. **You start mirrord.** `mirrord up` connects to sessions-manager and asks for a session with the service and environment in your `mirrord-up.yaml`.
3. **Sessions-manager pairs them.** It assigns both sides to the same session and gives each a single-use credential for it.
4. **Both sides open the session's data plane.** Sessions-manager relays mirrord traffic between them. From here, mirrord behaves as it does with a Kubernetes target: incoming requests matching your filter go to your local process, and its outgoing traffic leaves from the workload outside Kubernetes.

In the Operator-hosted mode, every connection goes through your cluster's API server to the Operator; see [Operator-Hosted Sessions-Manager](operator-hosted.md#how-it-works). In the MetalBear Cloud mode, both sides connect to the MetalBear-hosted sessions-manager over HTTPS: the workload authenticates with your API key, and you with the credentials `mirrord login` stored; see [MetalBear Cloud Sessions-Manager](metalbear-cloud.md#how-it-works).

***

## Limitations

* **No `replace` mode.** A serverless target can't be copied, so services run in `split` or `mirror` mode only. See [Serverless targets](../multiple-concurrent-sessions.md#serverless-targets).
* **One replica per session.** Your session is served by one registered replica of the service, such as one ECS task. If the service runs several, only requests that land on that replica can be stolen or mirrored. For testing, scale the service to one replica or route your test traffic to a specific one.
* **No queue splitting, DB branching, or copied targets.** [`feature.split_queues`](../../sharing-the-cluster/queue-splitting.md), [`feature.kafka_branch`](../../sharing-the-cluster/queue-splitting/kafka.md#kafka-branching), [`feature.db_branches`](../../sharing-the-cluster/db-branching.md), and [`feature.copy_target`](../copy-target.md) aren't supported with a `serverless/` target. `mirrord up` runs the service without them and prints a warning, even when its mode asks for queue splitting or DB branching. If a service's `config_patch` sets any of them, mirrord refuses to start the session, for example with `` `feature.db_branches` is not supported with serverless targets.`` Your local process uses the same databases and queues as the workload outside Kubernetes, so its writes reach the shared data.
* **Application container only.** Remote files and environment variables come from the application container the remote bootstrap is loaded into, not from the workload's other containers. Environment variables are read when the container starts, including those the platform injects from secrets; variables your application sets while running aren't included.
* **Operator-hosted mode:**
  * **Development traffic only.** Session traffic flows through your cluster's API server. That's fine for development, but don't use a mirrord session for load testing.
  * **Operator restarts end sessions.** Restarting or upgrading the Operator ends active sessions. The workload registers again on its own; rerun `mirrord up`.
  * **Long-lived connections.** Any proxy or firewall between the workload and the API server must not buffer responses and needs an idle timeout of at least 300 seconds.
* **MetalBear Cloud mode:**
  * **Development traffic only.** Session traffic is relayed through the MetalBear-hosted sessions-manager. Don't use a mirrord session for load testing.
  * **One region.** The MetalBear-hosted sessions-manager runs in AWS `us-east-1`.
  * **Sessions-manager restarts end sessions.** When MetalBear restarts or upgrades the hosted sessions-manager, active sessions end. The workload registers again on its own; rerun `mirrord up`.
  * **Long-lived connections.** Any proxy or firewall between either side and sessions-manager must not buffer responses and needs an idle timeout of at least 300 seconds.
