---
title: Targets
date: 2023-05-08T05:50:11.000Z
lastmod: 2023-05-08T05:50:11.000Z
draft: false
images: []
menu:
  docs:
    parent: reference
weight: 130
toc: true
tags:
  - oss
  - team
  - enterprise
description: Possible targets for mirrord and how to set them
---

## Overview

You can specify a target on your cluster for mirrord, giving your local application access to the remote target's network environment, file system and environment variables, according to the [configuration](https://metalbear.com/mirrord/docs/config). When a target is specified, a [mirrord-agent](architecture.md#mirrord-agent) pod will be created on the same node as the target pod. The several kinds of supported targets are detailed below. There are also multiple ways to specify a target for mirrord: you can do it in a configuration file, in an IDE dialog, or in the CLI with an argument or an environment variable.

{% hint style="info" %}
**Targeting more than one thing?** 

To run several *different* applications together as one managed group, see [`mirrord up`](../using-mirrord/multiple-concurrent-sessions.md).
To target every pod of one application that spans multiple workloads (or none), see [Targeting Pods by Label](../using-mirrord/targeting-pods-by-label.md).
{% endhint %}

## Possible targets

mirrord OSS supports the following Kubernetes objects as targets:

* Pods
* Deployments
* Argo Rollouts

In mirrord OSS, mirrord will always target a random pod when a workload with multiple pods is used as the remote target.

mirrord for Teams adds support for the following workloads:

* Jobs
* CronJobs
* StatefulSets
* ReplicaSets

mirrord for Teams also supports targeting:

* Services
* Labels

In mirrord for Teams, mirrord will always target all pods when a workload with multiple pods is used as the remote target.

When targeting a Service, mirrord resolves the Service to its backing pods via the Service's selector. Jobs and CronJobs require [`copy_target`](../using-mirrord/copy-target.md) to be enabled.

When targeting Labels, mirrord will target all pods that match the label selector. See [Targeting Pods by Label](../using-mirrord/targeting-pods-by-label.md) for how this behaves, version requirements, and current limitations.

Both in mirrord OSS and mirrord for Teams, if you don't name any specific container to be targeted, mirrord will pick the first container from the pod spec. Some containers, like service mesh proxies, will be automatically ignored.

You can specify a target namespace if the target should be found in that namespace instead of the namespace that is currently used by `kubectl`. See the different interfaces below for possible ways of specifying the target and its namespace.

## Specifying a target

There are multiple ways to specify a target. In all the possible interfaces for specifying a target, the basic format is `<resource-type>/<resource-name>` optionally followed by `/container/<container-name>`. So for specifying a target without specifying a container you can pass

```
deploy/<YOUR-DEPLOYMENT-NAME>
```

e.g. `deploy/lolz`,

or

```
pod/<YOUR-POD-NAME>
```

e.g. `pod/lolz-64698df9b7-6plq8`,

And for also specifying a container, you just add `/container/<CONTAINER-NAME>` at the end, e.g. `pod/lolz-64698df9b7-6plq8/container/main-container`.

### Using a [configuration file](https://metalbear.com/mirrord/docs/config)

The target path from the last section is set under the [`target.path`](https://metalbear.com/mirrord/docs/config#target.path) field. The target's namespace can be set under [`target.namespace`](https://metalbear.com/mirrord/docs/config#target.namespace). By default, the namespace currently specified in the local `kubeconfig` is used.

```json
{
  "target": {
    "path": "pod/lolz-64698df9b7-6plq8/container/main-container",
    "namespace": "lolzspace"
  }
}
```

### Using an IDE's dialog

If you are running one of mirrord's IDE extensions and you didn't specify a target via a configuration file, a dialog will pop up for you to pick a target. If you want to choose a target from a different namespace you can set a target namespace in the [configuration file](targets.md#using-a-configuration-file), and the dialog will then contain targets in that namespace. Choose the `No Target ("targetless")` option in the dialog in order to run without a target.

### Using a command line argument

If you are running mirrord from the command line, you can specify the target via `-t` and its namespace via `-n`, e.g. `mirrord exec -t deploy/lolz -n lolzspace my-app`. Values specified by command line arguments will be used even if other values are set in a configuration file or in environment variables.

### Using an environment variable

You can set the target using the environment variable `MIRRORD_IMPERSONATED_TARGET` and the target's namespace using the environment variable `MIRRORD_TARGET_NAMESPACE`. Values specified by environment variables will be used even if other values are set in a configuration file.

## Running without a target

When no target is specified, mirrord will start a _targetless_ agent. That can be useful when you want to connect to services from within the cluster, but you don't have any target that you want to "impersonate" - like when running an external utility or a new microservice. When running targetless, mirrord will forward any connections initiated by your application to be sent out of the cluster, but it will not mirror or steal incoming traffic, as a targetless agent is not connected to any Kubernetes service and does not expose any ports. This means that if your application binds a port and listens on it, that will all happen locally, on your machine. So if you're using a management program that exposes a web interface, you can have it listen for connections on `localhost`, and connect to remote services in the cluster.

If you're using a mirrord configuration file and want to run targetless, you can either leave the `target` key out completely or specify it explicitly. Note that if you want to skip the target dialog in the IDE plugins, you have to specify it explicitly. You can do it with the following configuration:

```json
{
  "target": "targetless"
}
```

In your IDE you can pick the `No Target ("targetless")` option in the target selection dialog (or just not make a selection). Moreover, you should make sure [the environment variable used to specify a target](targets.md#using-an-environment-variable) isn't set (or is set to an empty value).

> **Note:** In order to set the namespace the agent is going to be created in, set the agent namespace, not the target namespace. That value can be set via the [`agent.namespace` configuration file field](https://metalbear.com/mirrord/docs/config#agent.namespace), the `-a` CLI argument, or the `MIRRORD_AGENT_NAMESPACE` environment variable.
