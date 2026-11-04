---
title: "Operator-Hosted Sessions-Manager"
description: "Run sessions-manager inside the mirrord Operator, so developers can target serverless workloads such as Amazon ECS tasks through your cluster's API server."
tags:
  - experimental
  - enterprise
---

# Operator-Hosted Sessions-Manager

{% hint style="warning" %}
The Operator-hosted sessions-manager is **experimental**, and may change without notice. It requires Operator **3.223.0 or later**; see [Required versions](README.md#required-versions).
{% endhint %}

In the Operator-hosted [deployment mode](README.md#deployment-modes), sessions-manager runs inside the mirrord Operator. Enabling it is a Helm value: there's no additional deployment, service or load balancer, and the Operator isn't exposed outside the cluster. At a glance, the setup is:

1. Enable sessions-manager with the `operator.sessionsManager` Helm value ([Enable sessions-manager](#enable-sessions-manager)).
2. Check that developers are bound to one of the built-in Operator roles ([Developer access](#developer-access)).
3. Connect each workload, which maps its identity into the cluster and binds it to the `mirrord-operator-sessions-manager-agent` ClusterRole ([Connecting workloads](#connecting-workloads)).

## Prerequisites

1. A Kubernetes cluster with mirrord Operator **3.223.0 or later** installed through its Helm chart, and a mirrord Operator license. For initial installation and licensing, follow the [Operator installation guide](../../managing-mirrord/operator.md#installation). The step-by-step workload guides cover EKS; other clusters work through a [kubeconfig context](#connection-variables).
2. `kubectl` and `helm`, with permission to manage the Operator Helm release and cluster RBAC.

---

## How it works

{% hint style="info" %}
This section is for whoever reviews the setup, such as your security team. To start the setup, skip to [Enable sessions-manager](#enable-sessions-manager).
{% endhint %}

```
 developer machine                     EKS VPC                                workload network
┌───────────────────┐  kubeconfig   ┌───────────────────────────────┐      ┌───────────────────────────┐
│ mirrord exec      │─────────────► │ EKS API server endpoint       │      │ application container     │
│ (local process)   │               │   │ aggregated API            │      │ + mirrord remote          │
└───────────────────┘               │   ▼                           │      │   bootstrap (LD_PRELOAD)  │
                                    │ mirrord Operator              │      └─────────────┬─────────────┘
                                    │   (sessions-manager inside)   │                    │ HTTPS, Kubernetes
                                    └───────────────▲───────────────┘                    │ identity
                                                    └────────────────────────────────────┘
```

Every caller reaches sessions-manager the same way it reaches every other Operator feature: through the cluster's API server and the Operator's aggregated API. Both sides authenticate as Kubernetes identities, access is granted with ordinary Kubernetes RBAC, and requests are recorded in the cluster's audit log. There are no shared secrets to create, distribute or rotate.

* **Developers** authenticate with their kubeconfig, exactly like for any other Operator feature.
* **Workloads** authenticate with their platform identity, mapped to a Kubernetes identity. For ECS, the task's IAM role is mapped with an EKS access entry; see [How ECS tasks authenticate](ecs-operator-hosted.md#how-ecs-tasks-authenticate).

The Operator serves sessions-manager as cluster-scoped resources on its existing `operator.metalbear.co/v1alpha1` API. Each side registers on a long-lived Server-Sent Events stream and waits to be paired, then opens a WebSocket to its session, through which the Operator relays mirrord traffic.

Developers and workloads register on separate resources, and the chart grants them through separate ClusterRoles, so RBAC decides which side an identity may act as: a developer can't register as a workload and be paired with other developers' sessions. See [Developer access](#developer-access) and [Connecting workloads](#connecting-workloads) for the exact rules. A workload's registration is named `<environment>.<service>`, so its access can also be limited to specific services ([Limiting a workload to specific services](#limiting-a-workload-to-specific-services)).

The Operator receives the caller's identity from the API server, like it does for every other Operator request, and implements no authentication of its own. Each data-plane connection is also bound to its session by a single-use credential delivered with the assignment, so an identity allowed to connect to sessions can't attach to another caller's session.

Both sides only make outbound HTTPS connections, and only to the API server. Neither needs a network path to the other.

---

## Enable sessions-manager

Set `operator.sessionsManager: true` in your Operator Helm values:

```yaml
operator:
  sessionsManager: true
```

Upgrade your existing release using the standard MetalBear Helm repository:

```bash
helm repo add metalbear https://metalbear-co.github.io/charts
helm repo update metalbear

helm upgrade mirrord-operator metalbear/mirrord-operator \
  --namespace mirrord \
  --version 3.223.0 \
  -f values.yaml
```

Use your maintained `values.yaml`, including your license or cloud authentication settings and any other configuration your release needs, with `operator.sessionsManager: true` added. Don't replace it with a fresh defaults file. The example pins version `3.223.0`; you can use a later supported release instead.

{% hint style="info" %}
If your values override the Operator image or tag, update the override to version **3.223.0 or later** too. For mirrored images, follow [Using an Internal Registry](../../managing-mirrord/operator.md#using-an-internal-registry-optional). The chart's default image needs no override.
{% endhint %}

### Confirm sessions-manager is served

Confirm the Operator now serves the three resources:

```bash
kubectl api-resources --categories=mirrord-serverless
# NAME                          SHORTNAMES   APIVERSION                       NAMESPACED   KIND
# serverlessagentassignments                 operator.metalbear.co/v1alpha1   false        ServerlessAgentAssignment
# serverlessclientassignments                operator.metalbear.co/v1alpha1   false        ServerlessClientAssignment
# serverlessdataplanes                       operator.metalbear.co/v1alpha1   false        ServerlessDataPlane
```

## Developer access

Enabling sessions-manager grants `proxy` on `serverlessclientassignments` (the **client** side: developers) and `get` on `serverlessdataplanes` in the chart's `mirrord-operator-user`, `mirrord-operator-ci` and `mirrord-operator-user-basic` ClusterRoles. Developers already bound to one of those roles need no further RBAC changes, and nothing beyond their existing kubeconfig for the cluster:

```bash
aws eks update-kubeconfig --name <CLUSTER_NAME> --region <REGION>
```

If you grant Operator access with your own roles instead, add the two rules above to them. For the developer's `mirrord.json`, see [Using mirrord with a Serverless Workload](README.md#using-mirrord-with-a-serverless-workload).

A developer can check their access with their own kubeconfig:

```bash
kubectl auth can-i proxy serverlessclientassignments.operator.metalbear.co   # yes (client: developers)
kubectl auth can-i get serverlessdataplanes.operator.metalbear.co            # yes
kubectl auth can-i proxy serverlessagentassignments.operator.metalbear.co    # no  (agent: workloads)
```

To check a developer's access as a cluster admin, add `--as <USERNAME>`, and `--as-group <GROUP>` for each group they're bound through. The last command must answer `no`, unless the identity is also a cluster admin: a developer who can register as a workload could be paired with other developers' sessions.

## Connecting workloads

Enabling sessions-manager also creates the `mirrord-operator-sessions-manager-agent` ClusterRole. It grants only `proxy` on `serverlessagentassignments` (the **agent** side: workloads) and `get` on `serverlessdataplanes`, and none of the user permissions: a workload bound to it can't read, list or modify any other resource in the cluster, and can't register as a developer.

Each workload authenticates as a Kubernetes identity bound to this role. How its identity is mapped, and how it reaches the API server, depends on the platform:

* [Connecting ECS to the Operator](ecs-operator-hosted.md)

### Connection variables

The remote bootstrap picks how it reaches the Operator from the variables set on the workload. Set exactly one of `MIRRORD_OPERATOR_API_URL` and `MIRRORD_OPERATOR_KUBE_CONTEXT`, and not `MIRRORD_SESSIONS_MANAGER_URL`, which points the bootstrap at a sessions-manager outside the Operator; the bootstrap refuses to start when more than one is set.

| Variables | Authenticates as | Use it for |
| --- | --- | --- |
| `MIRRORD_OPERATOR_API_URL`, `MIRRORD_OPERATOR_EKS_CLUSTER_NAME` and `MIRRORD_OPERATOR_API_CA_DATA` | The workload's AWS identity, such as the ECS task role, mapped into an EKS cluster with an access entry. | Workloads on AWS connecting to EKS. No secrets are added to the workload. See [Connecting ECS to the Operator](ecs-operator-hosted.md). |
| `MIRRORD_OPERATOR_KUBE_CONTEXT` | Whatever that context's user is in the workload's kubeconfig, such as a ServiceAccount token or an `exec` credential plugin. | Any other cluster, or credentials your platform already provides as a kubeconfig. |
| `MIRRORD_OPERATOR_API_URL` alone | Nothing: requests are sent without credentials. | Only behind something that authenticates on the workload's behalf, such as a `kubectl proxy` sidecar. |

With `MIRRORD_OPERATOR_KUBE_CONTEXT`, the bootstrap reads the kubeconfig the usual way, from `KUBECONFIG` or `~/.kube/config`, and takes the server, certificate authority and credentials from the named context. Mount the kubeconfig into the application container and set `KUBECONFIG` to its path. If the context uses an `exec` plugin, such as `aws eks get-token` or `gke-gcloud-auth-plugin`, the plugin must be installed in the application container. For example, to authenticate as a ServiceAccount bound to the chart's role:

```bash
kubectl create namespace mirrord-workloads
kubectl -n mirrord-workloads create serviceaccount payments
kubectl create clusterrolebinding mirrord-workload-payments \
  --clusterrole mirrord-operator-sessions-manager-agent \
  --serviceaccount mirrord-workloads:payments
kubectl -n mirrord-workloads create token payments --duration 24h
```

and reference the token from the context's user in the workload's kubeconfig. A ServiceAccount token is a secret: store the kubeconfig in your platform's secret store rather than in plain configuration, and rotate it before it expires.

{% hint style="info" %}
Bind the chart's role rather than defining your own, unless you need to limit a workload to specific services: it stays in sync with what the Operator requires when you upgrade the chart.
{% endhint %}

### Limiting a workload to specific services

The chart's role lets a workload register under any environment and service. To limit an identity to specific ones, bind it to your own ClusterRole instead, listing each `<environment>.<service>` it may register as in `resourceNames`:

```yaml
apiVersion: rbac.authorization.k8s.io/v1
kind: ClusterRole
metadata:
  name: mirrord-sessions-manager-agent-staging-api
rules:
  - apiGroups: [operator.metalbear.co]
    resources: [serverlessagentassignments]
    resourceNames: [staging.api]
    verbs: [proxy]
  - apiGroups: [operator.metalbear.co]
    resources: [serverlessdataplanes]
    verbs: [get]
```

An identity bound to this role can register as service `api` in environment `staging`, and gets `403 Forbidden` for any other. In resource names, the environment and service are lowercased, with every run of other characters replaced by `-`: service `Payments API` in environment `Staging EU` registers as `staging-eu.payments-api`. Data planes are named by per-session IDs, so the `serverlessdataplanes` rule can't be narrowed; each connection is already bound to its session by its single-use credential.

---

## Operating notes

* **Operator restarts and upgrades** end active sessions. Workloads register again on their own; developers rerun `mirrord exec`. If connections are cut during rollouts, raise the chart's `operator.terminationGracePeriodSeconds` (default `25`) to give sessions time to close cleanly.
* **Traffic path**: session traffic flows through the API server. It's suited to development traffic, not to load testing through a mirrord session.
* **Long-lived connections**: a session holds its HTTPS connections open for as long as the developer runs it. The Operator sends a keepalive on each registration stream every 15 seconds, and data-plane connections carry mirrord's own periodic traffic.

{% hint style="warning" %}
Any firewall or proxy between a workload and the API server needs an idle timeout of at least 300 seconds and must not buffer responses.
{% endhint %}

## Troubleshooting

| Symptom | Likely cause |
| --- | --- |
| `404 Not Found`, or mirrord says the Operator doesn't serve sessions-manager | `operator.sessionsManager` isn't enabled, or the Operator image is older than **3.223.0**. Check any image or tag overrides in your Helm values. |
| `403 Forbidden` on `serverlessclientassignments` for a developer | The developer isn't bound to one of the built-in user roles, or a custom role lacks the two rules ([Developer access](#developer-access)). |
| `403 Forbidden` on `serverlessagentassignments` for a workload bound to a custom role | The role's `resourceNames` doesn't list the workload's `<environment>.<service>`, in its lowercased form ([Limiting a workload to specific services](#limiting-a-workload-to-specific-services)). |
| `MIRRORD_SESSIONS_MANAGER_URL, MIRRORD_OPERATOR_API_URL and MIRRORD_OPERATOR_KUBE_CONTEXT are mutually exclusive` | The workload sets more than one of them ([Connection variables](#connection-variables)). |
| A workload is refused with `HTTP control plane returned 401 Unauthorized`, and never reaches the Operator | None of the [connection variables](#connection-variables) are set on the workload, so it registers with the MetalBear Cloud sessions-manager instead. |
| mirrord prints `Connecting through the MetalBear Cloud sessions-manager` | `mirrord.json` doesn't set `"operator": true` ([Using mirrord with a Serverless Workload](README.md#using-mirrord-with-a-serverless-workload)). |
| `Failed to load kubeconfig context` | `KUBECONFIG` doesn't point at the mounted kubeconfig, or it has no context named `MIRRORD_OPERATOR_KUBE_CONTEXT`. |
| The workload registers but the developer is never paired | The workload's service and environment names don't match `target.path` and `target.namespace` in `mirrord.json`. |

For workload-side problems, see the troubleshooting section of your platform's connection guide, such as [Connecting ECS to the Operator](ecs-operator-hosted.md#troubleshooting). To see which sessions-manager a workload registers with, and why, set `MIRRORD_AGENT_RUST_LOG=info` on it. The bootstrap then logs its choice when it starts, for example `Registering with the sessions-manager hosted by the mirrord operator, selected by MIRRORD_OPERATOR_API_URL`.

To see registrations and sessions, follow the Operator's logs:

```bash
kubectl -n mirrord logs deployment/mirrord-operator -f | grep -i session
```
