---
title: Node Upgrades and Scale-Down
date: 2026-10-01T00:00:00.000Z
lastmod: 2026-10-01T00:00:00.000Z
draft: false
images: []
linktitle: Node Upgrades and Scale-Down
menu: null
docs: null
teams: null
weight: 508
toc: true
tags:
  - team
  - enterprise
description: What happens to active mirrord sessions when Karpenter, Cluster Autoscaler, a node pool upgrade or kubectl drain removes a node
---

{% hint style="info" %}
This page is relevant for users on the Team and Enterprise pricing plans.
{% endhint %}

Nodes go away when you upgrade Kubernetes, when your cloud provider replaces nodes, or when Karpenter or Cluster
Autoscaler removes nodes. Before a tool removes a node, it drains the node. To drain a node, the tool evicts the pods on
the node. The controllers of these pods make new pods on other nodes. This page tells you what a drain does to mirrord
pods and to the sessions that use them, and how to find the sessions that use a node.

On this page, mirrord pods are the agent pods and the copy pods that the mirrord Operator makes for sessions.

| Tool | Do mirrord pods stop the node from going away? | What to do |
| --- | --- | --- |
| [Karpenter](#karpenter) (drift, consolidation, expiration) | No, from mirrord Operator `3.214.0`. With earlier versions, agent pods stop drift and consolidation until their sessions end. | The upgrade of the Operator can end active sessions. Upgrade the Operator to `3.214.0` or later before you replace the nodes. |
| [Node pool upgrade of your cloud provider](#node-pool-upgrades-of-your-cloud-provider) (GKE, EKS, AKS) | No. If Karpenter manages the nodes (for example, AKS node auto-provisioning), the upgrade is a Karpenter drift. | Nothing. If Karpenter manages the nodes, see [Karpenter](#karpenter). |
| [Cluster Autoscaler](#cluster-autoscaler) scale-down | Yes, in every Operator version, until the sessions end. | Wait for the sessions to end, or [end them](#find-the-sessions-that-use-agents-on-a-node). |
| [`kubectl drain`](#kubectl-drain) | Targeted agent pods: no, from mirrord Operator `3.214.0`. Targetless agent pods and copy pods: yes, unless you use `--force`. | Upgrade the Operator. For targetless agent pods and copy pods, see [`kubectl drain`](#kubectl-drain). |

A PodDisruptionBudget can also stop a drain. See
[PodDisruptionBudgets that stop a drain](#poddisruptionbudgets-that-stop-a-drain). For the effect on active sessions,
see [What happens to an active session](#what-happens-to-an-active-session).

## Agent pods and nodes

When a user starts a session with a target, the mirrord Operator makes sure that each ready pod of the target has an
agent pod. Sessions with the same target share these agent pods. When no session uses an agent pod, the Operator deletes
it.

An agent works inside the network namespace of its target pod, so it must run on the same node as its target pod. The
agent pod has a `kubernetes.io/hostname` node selector or a `spec.nodeName` field that points to that node. It cannot
move to a different node, and no controller replaces it on a different node.

Agent pods have the label `operator.metalbear.co/type=agent`. Targeted agent pods are in the namespace of the Operator
(`mirrord` by default), unless you set `agent.extraConfig.namespace` in the Helm values.

These agents and pods work differently from targeted agent pods:

- A targetless agent pod is in the namespace of its session. It can run on any node, and it has no owner.
- An ephemeral agent (the `ephemeral` agent setting) is a container in the target pod, not a pod. It stops together
  with the target pod.
- A copy pod (the `copy_target` setting) is a copy of the target pod that the Operator makes for a session. Its name
  starts with `mirrord-copy-`, and it has the label `operator.metalbear.co/type=copy-target` (`mc-copy-target` in a
  multi-cluster session). It has no owner. It has the labels, annotations and tolerations of its target pod.

## What happens to an active session

When a tool drains a node, Kubernetes evicts the target pods on the node. A targeted agent stops when its target pod
stops, when the drain evicts the agent pod, or when the node is deleted. The session does not end at once:

1. mirrord loses the connection to the agent.
2. If other ready pods of the target have agents, the session continues with them immediately.
3. The Operator watches the target. When a new pod of the target becomes ready, for example on a different node, the
   Operator starts an agent for it, and the session uses it.
4. If the session has no agent for 60 seconds, the Operator closes the session. mirrord stops the local process of the
   user, and the log of the mirrord internal proxy shows this error:

   ```
   agent closed connection with error: the session exceeded the configured 60000ms timeout on no live mirrord-agents
   ```

   60 seconds is the default. To change it, see
   [Keep sessions alive during a drain](#keep-sessions-alive-during-a-drain).

The result depends on the target of the session:

| Target | Result of a node drain |
| --- | --- |
| A workload (for example, a Deployment) that has ready pods on other nodes | The session continues. |
| A workload that has all its ready pods on the drained node | The session continues if a new pod becomes ready and its agent starts in less than 60 seconds. If not, the session ends. |
| A pod (`pod/<name>`) | The session ends after 60 seconds. The evicted pod does not come back. |
| A pod of a StatefulSet (`pod/<name>`) | The session continues if the pod becomes ready again in less than 60 seconds. Kubernetes makes a new pod with the same name. |
| A copy of the target (`copy_target`), without `scale_down` and without queue splitting | The session uses the pods of the original target, the same as a session without a copy. The eviction of the copy pod does not affect the session. |
| A copy of the target (`copy_target`) with `scale_down`, with queue splitting, or with a Job or CronJob target | The session uses only the copy pod. The session ends 60 seconds after the copy pod is evicted. No controller makes a new copy pod. |
| No target (targetless) | The session continues. mirrord connects again, and the Operator starts a new targetless agent. |

When a session moves to a different agent, the user can see these effects:

- Incoming connections that came through the lost agent close. The session keeps its port subscriptions, so new
  connections come through the new agent.
- Outgoing connections that went through the lost agent close.
- The session uses one of its agents for file operations, DNS and outgoing traffic. If the session loses this agent:
  - Operations that are in progress fail. For example, a file read fails with an input/output error.
  - While the session has no agent, new operations also fail.
  - Remote files that were open before the session moved are not valid anymore.
  - After the session moves, file operations, DNS and outgoing traffic come from a different pod of the target.
- The local process keeps the environment variables that it got when it started.

In a multi-cluster session, the session in each cluster recovers separately. If the session in one cluster has no agent
for 60 seconds, the full multi-cluster session ends.

## Keep sessions alive during a drain

- Tell users to target a workload, for example `deployment/<name>`, and not one pod.
- Give the targets more than one replica and a
  [PodDisruptionBudget](https://kubernetes.io/docs/tasks/run-application/configure-pdb/), so that a ready pod stays
  during the drain.
- If new target pods and their agents need more than 60 seconds to become ready, for example because Karpenter must
  start a new node first, increase `noPodTargetsSessionTimeoutMillis` in the Helm values of the Operator:

  ```yaml
  operator:
    ## How long a session can live with no agent. Default: 60000.
    noPodTargetsSessionTimeoutMillis: 300000
  ```

## PodDisruptionBudgets that stop a drain

The Helm chart makes no PodDisruptionBudget. But two PodDisruptionBudgets that you make can stop a drain:

- A [PodDisruptionBudget for the Operator](high-availability.md#recommended-poddisruptionbudget). If only one
  Operator replica runs, this PodDisruptionBudget stops the drain of the node that runs the Operator. To prevent
  this, run at least two Operator replicas. A drain of that node also restarts the Operator, and this can affect
  sessions (see [High Availability](high-availability.md)).
- A PodDisruptionBudget for a target. A copy pod has the labels of its target pod, so a PodDisruptionBudget of the
  target with an integer `minAvailable` also applies to the copy pod. With `scale_down`, the copy pod can be the only
  ready pod. Then the PodDisruptionBudget stops the eviction of the copy pod until the session ends, or until the
  tool stops waiting. For example, a GKE node pool upgrade waits for at most one hour. `--force` does not change
  this.

## Find the sessions that use agents on a node

Sessions do not record their agent pods or nodes. To find the sessions that use agents on a node, find the target pod of
each agent pod on the node. Then find the sessions with these targets. The commands below use
[jq](https://jqlang.org/).

1. List the agent pods on the node, with their target pods. A targeted agent pod gets the ID of its target container
   as an argument. The command finds the pod that has a container with this ID:

   ```bash
   NODE=<node name>
   kubectl get pods --all-namespaces --field-selector spec.nodeName="$NODE" --output json | jq --raw-output '
     .items as $pods
     | ([$pods[] | . as $pod | .status.containerStatuses[]? | select(.containerID)
         | {key: (.containerID | sub("^[^:]+://"; "")),
            value: ["\($pod.metadata.namespace)/\($pod.metadata.name)", .name,
                    ($pod.metadata.ownerReferences[0] // {} | if .kind then "\(.kind)/\(.name)" else "-" end)]}]
       | from_entries) as $containers
     | $pods[]
     | select(.metadata.labels["operator.metalbear.co/type"] == "agent")
     | (.spec.containers[] | select(.name == "mirrord-agent") | .args) as $args
     | ($args | index("--container-id")) as $i
     | ["\(.metadata.namespace)/\(.metadata.name)"]
       + if $i == null then ["targetless", "-", "-"]
         else $containers[$args[$i + 1]] // ["target not found", "-", "-"] end
     | @tsv' | column -t -s $'\t'
   ```

   The columns are the agent pod, the target pod, the target container and the owner of the target pod. Example
   output:

   ```
   mirrord/mirrord-agent-wpj9f  shop/checkout-8488cdbb6f-hc5sn  app  ReplicaSet/checkout-8488cdbb6f
   shop/mirrord-agent-jl8bv     targetless                      -    -
   ```

   The list does not show:

   - An agent pod that is still starting. Such a pod possibly has no node yet.
   - Ephemeral agents. A target pod with an ephemeral agent has an ephemeral container with a name that starts with
     `mirrord-agent-`.
   - Copy pods that no agent uses. To list all the copy pods on the node, run this command:

     ```bash
     kubectl get pods --all-namespaces --field-selector spec.nodeName="$NODE" \
       --selector 'operator.metalbear.co/type in (copy-target,mc-copy-target)'
     ```
2. List the active sessions, with their targets and their users:

   ```bash
   kubectl get mirrordclustersessions.mirrord.metalbear.co --output json | jq --raw-output '
     .items[]
     | select(.status.closed == null)
     | [.metadata.name, .spec.namespace,
        (.spec.target
         | if . == null then "targetless"
           elif .labelSelector then "labels \(.labelSelector.matchLabels // {} | to_entries | map("\(.key)=\(.value)") | join(","))"
           else "\(.kind)/\(.name)" end),
        (.status.copyTarget.copiedPodStatus.name // "-"),
        .spec.owner.username, .spec.owner.k8sUsername]
     | @tsv' | column -t -s $'\t'
   ```

   The columns are the session ID, the namespace, the target, the copy pod, the user name and the Kubernetes user.
   Example output:

   ```
   7cdee5d05570b248  shop  Deployment/checkout  -  Jane Doe  jane@example.com
   26145e46dfab350f  shop  targetless           -  John Doe  john@example.com
   ```

   `mirrord operator status` shows the same sessions, with the session ID in uppercase.
3. Match the pods from step 1 to the sessions from step 2. The session must be in the namespace of the target pod. For
   a targetless agent, the session must be in the namespace of the agent pod. Then use the target of the session:

   | Target of the session | The session uses the agent if |
   | --- | --- |
   | `Pod/<name>` | The target pod has this name. |
   | `Deployment/<name>` or `Rollout/<name>` | The owner of the target pod is a ReplicaSet that this workload owns. To see the owner of a ReplicaSet, run `kubectl get replicaset <ReplicaSet name> --namespace <namespace> --output jsonpath='{.metadata.ownerReferences[0].name}'`. |
   | `StatefulSet/<name>` or `ReplicaSet/<name>` | The owner of the target pod is this workload. |
   | `Service/<name>` | The labels of the target pod match the selector of the Service. To see the labels of the target pod, run `kubectl get pod <pod name> --namespace <namespace> --show-labels`. |
   | `labels ...` | The target pod has all these labels. |
   | `targetless` | The agent is targetless. |

   If the target pod is a copy pod, find this copy pod in step 2. The session on that line uses the agent. A copy pod
   that no agent uses also belongs to the session that shows it in step 2.

   All the targetless sessions in a namespace share one targetless agent pod. So all of them use the agent, and the
   agent pod stays until all of them end.

For a multi-cluster session, do steps 1 and 2 in the cluster that has the node. The `spec.multiClusterParentName` field
of the session resource gives the ID of its multi-cluster session.

To end a session, ask its user to stop it. Or, end the session yourself with its ID from step 2. To end a multi-cluster
session, you need mirrord CLI `3.234.0` or later. Use the ID of the multi-cluster session, and run the command in the
primary cluster.

```bash
mirrord operator session kill --id <session ID>
```

For more information, see [Managing Sessions](../sharing-the-cluster/sessions.md).

## Karpenter

On AKS, Karpenter is also known as node auto-provisioning (NAP).

Before Karpenter disrupts a node for drift or consolidation, it simulates how to move all the pods of the node to other
nodes. Karpenter does not include pods that a node owns in this simulation, and it does not evict them when it drains
the node. Karpenter does not do this simulation for expiration, so agent pods never stop the expiration of a node.

From mirrord Operator `3.214.0`, the node owns each targeted agent pod. The agent pod has an owner reference with
`apiVersion: v1`, `kind: Node` and `controller: true`. `blockOwnerDeletion` is not set, so the agent pod never stops the
deletion of its node. When Karpenter drains the node, it evicts the target pods, and each agent stops with its target
pod or with the node.

Targetless agent pods and copy pods have no node selector that only their node matches, so they do not stop the
simulation. Karpenter evicts them when it drains the node. But some of these pods tolerate the `karpenter.sh/disrupted`
taint, for example a copy pod of a target that tolerates all taints. Karpenter does not evict these pods, and they stop
when the node stops. A copy pod of a target with the `karpenter.sh/do-not-disrupt: "true"` annotation stops Karpenter
drift and consolidation, the same as the target pod.

Database branch pods do not come from a target, so they stop Karpenter only when you annotate them yourself: set
`karpenter.sh/do-not-disrupt: "true"` under `dbPod.annotations` of each
[`<engine>BranchConfig`](../sharing-the-cluster/db-branching.md#branch-pod-labels-and-annotations) in the operator's
Helm values. Preview pods copy the target's pod template, so a target that already carries the annotation protects its
previews too; for targets that do not, set it for every preview pod under
[`operator.preview.annotations`](../use-cases/preview-environments.md#pod-labels-and-annotations). Without the
annotation, Karpenter evicts these pods when it consolidates the node, and the session loses its branch database or its
preview pod.

### Check that agent pods have an owner

If Karpenter still does not disrupt a node after you upgrade, check the owner of the agent pods:

```bash
kubectl get pods --all-namespaces --selector operator.metalbear.co/type=agent \
  --output custom-columns='NAMESPACE:.metadata.namespace,AGENT:.metadata.name,NODE:.spec.nodeName,OWNER:.metadata.ownerReferences[*].kind'
```

A targeted agent pod must have `Node` in the `OWNER` column. Targetless agent pods have `<none>`.

If the Operator cannot read the node when it makes the agent pod, it makes the pod with no owner and logs this warning:
`failed to get the node of a targeted agent, the agent pod will have no owner`. The Operator ClusterRole in the Helm
chart has the necessary `get` permission on nodes. If you do not use this ClusterRole, give the Operator this
permission.

### Earlier Operator versions

Agent pods made by mirrord Operator `3.213.0` or earlier have no owner. If such an agent pod has a
`kubernetes.io/hostname` node selector, no other node matches the selector, and the Karpenter simulation fails.
Karpenter then keeps the node until all the sessions that use the agent end. On the node, Karpenter shows
`DisruptionBlocked` events (for drift) or `Unconsolidatable` events (for consolidation), with a message such as:

```
Not all pods would schedule, mirrord/mirrord-agent-xxxxx => incompatible requirements, key kubernetes.io/hostname In [<node>] not in [hostname-placeholder-...]
```

To find these events:

```bash
kubectl get events --all-namespaces --field-selector reason=DisruptionBlocked
kubectl get events --all-namespaces --field-selector reason=Unconsolidatable
```

{% hint style="warning" %}
The upgrade of the Operator can end active sessions. On the Enterprise plan, the Operator can restore sessions after it
restarts. See [High Availability](high-availability.md).
{% endhint %}

To fix this, upgrade the Operator. When the new Operator starts, it deletes the agent pods of the earlier Operator. All
the new agent pods have the node as owner.

## Node pool upgrades of your cloud provider

A node pool upgrade of your cloud provider, for example a GKE surge upgrade, an EKS managed node group update or an AKS
node image upgrade, cordons and drains each node with the Eviction API. mirrord pods do not stop the drain, unless a
PodDisruptionBudget applies to them (see
[PodDisruptionBudgets that stop a drain](#poddisruptionbudgets-that-stop-a-drain)). For the effect on active sessions,
see [What happens to an active session](#what-happens-to-an-active-session).

If Karpenter manages the nodes, for example with AKS node auto-provisioning, Karpenter replaces the nodes as drift. See
[Karpenter](#karpenter).

## Cluster Autoscaler

When Cluster Autoscaler scales down, it only removes a node if it can move all the pods of the node. A mirrord agent pod
or copy pod stops the scale-down of its node, in every Operator version.

By default, Cluster Autoscaler only moves pods that a ReplicationController, ReplicaSet, Job or StatefulSet controls.
mirrord pods have no such controller:

- Targetless agent pods and copy pods have no controller.
- From mirrord Operator `3.214.0`, the controller of a targeted agent pod is its node. Earlier targeted agent pods have
  no controller.

The `cluster-autoscaler.kubernetes.io/safe-to-evict: "true"` annotation does not help for targeted agent pods, because
no other node matches their node selector. A copy pod has the annotations of its target pod. If the target pod has
this annotation, Cluster Autoscaler can evict the copy pod. A session that uses only the copy pod then ends.

Cluster Autoscaler logs the reason with a message such as
`Node <node> cannot be removed: mirrord/mirrord-agent-xxxxx is not replicated`. On GKE, the
[cluster autoscaler visibility logs](https://cloud.google.com/kubernetes-engine/docs/how-to/cluster-autoscaler-visibility)
show a `noScaleDown` entry with the reason `no.scale.down.node.pod.not.backed.by.controller`.

Cluster Autoscaler removes the node after all the sessions that use its mirrord pods end and the Operator deletes these
pods. To remove the node sooner,
[find the sessions that use agents on the node](#find-the-sessions-that-use-agents-on-a-node) and end them.

## kubectl drain

From mirrord Operator `3.214.0`, `kubectl drain` evicts targeted agent pods the same as other pods, because the node is
their controller. With earlier Operator versions, `kubectl drain` stops with an error that contains this text:

```
declare no controller (use --force to override): mirrord/mirrord-agent-xxxxx
```

To fix this, upgrade the Operator.

Targetless agent pods and copy pods have no controller in every Operator version, so `kubectl drain` needs `--force`
to evict them.

{% hint style="warning" %}
`--force` also deletes all other pods on the node that have no controller.
{% endhint %}

Before you use `--force`, do a dry run to see which pods `kubectl drain` evicts:

```bash
kubectl drain <node> --ignore-daemonsets --force --dry-run=server
```
