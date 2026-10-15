---
title: Copying a Target
date: 2024-01-10T13:37:00.000Z
lastmod: 2024-01-10T13:37:00.000Z
draft: false
images: []
linktitle: Copying a Target
menu: null
docs: null
teams: null
weight: 170
toc: true
tags:
  - team
  - enterprise
description: Making mirrord copy a target and use the copy instead of the original
---

When you set the [`copy_target`](https://metalbear.com/mirrord/docs/config#feature.copy_target) configuration field, instead of using the [target](../reference/targets.md) of the run directly, mirrord will create a new pod using the pod spec of the original target, and use that new pod as a target.

{% hint style="info" %}
This feature is available to users on the Team and Enterprise pricing plans.
{% endhint %}

This can be useful when you want to run your application with access to the resources and I/O of a target that isn't reliable, for example because the target pod keeps crashing, or because it is managed by a [Job](https://kubernetes.io/docs/concepts/workloads/controllers/job/) and might terminate before you are done debugging your application with mirrord.

## Health Checks

The new, copied pod will not have any [liveness, readiness or startup probes](https://kubernetes.io/docs/tasks/configure-pod-container/configure-liveness-readiness-startup-probes/) even if the original pod spec does define them. This means you can [steal](https://metalbear.com/mirrord/docs/config#feature.network.incoming.mode) traffic without having to also answer those probes. This might come in handy when debugging with breakpoints with stolen traffic. Without `copy_target`, if you linger too long on a breakpoint, the application might miss some probes, which could cause a target pod to restart.

## Replacing a Whole Workload Using `scale_down`

When the [`scale_down`](https://metalbear.com/mirrord/docs/config#feature.copy_target.scale_down) option is set, mirrord will [scale](https://kubernetes.io/docs/concepts/workloads/controllers/deployment/#scaling-a-deployment) the target workload down to zero, effectively replacing all existing pods of that workload by the one new copied pod, that is then used as the target for the mirrord run. This feature is supported with Deployment, Argo Rollout, StatefulSet, and ReplicaSet (owned by either a Deployment or an Argo Rollout) targets.

The scale down feature can be useful e.g. when a workload reads from a queue. By scaling it down to zero, the application you run with mirrord does not have to compete with the workload's pods for queue items.

If KEDA scales the workload, set `operator.manageKedaScaledObjects` in the operator's Helm values (operator `3.215.0` or later) to keep it at zero: the operator pauses KEDA over the workload for the length of the session.

Only one mirrord session can scale down a workload at the same time. If you try to scale down a workload that is already being scaled down in another mirrord session (by you or by a teammate), mirrord will display an error and exit.

You can see active copied targets by running `mirrord operator status`. When there are no active copy targets, the relevant part of the output will say "_No active copy targets_".

When there are active copy targets, the relevant section of the output will look like this:

```
Active Copy Targets:
+-------------------------------+-----------+------------------------------+-------------+
| Original Target               | Namespace | Copy Pod Name                | Scale Down? |
+-------------------------------+-----------+------------------------------+-------------+
| deployment/py-serv-deployment | default   | mirrord-copy-job-wd8kj-2gvd4 | *           |
+-------------------------------+-----------+------------------------------+-------------+
```

With an asterisk marking copy targets that are also scaling down their original target.

Please note however that you don't necessarily have to check if a target is already being scaled down, as trying to scale it down again will not interrupt the ongoing session, it will just result in your new run exiting with an error.

## Ignoring Specific Containers

You can use [`exclude_containers`](https://metalbear.com/mirrord/docs/config#feature.copy_target.exclude_containers) and/or [`exclude_init_containers`](https://metalbear.com/mirrord/docs/config#feature.copy_target.exclude_init_containers) settings to ignore certain containers when creating the copy target. The containers will not be present in the copied pod's spec.

Please note that excluding the target container does not have any effect.
