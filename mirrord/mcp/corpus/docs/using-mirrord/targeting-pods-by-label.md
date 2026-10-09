---
title: Targeting Pods by Label
description: Target every pod that shares a Kubernetes label from a single mirrord session
tags:
  - team
  - enterprise
---

# Introduction

Applications sometimes span more than one Kubernetes workload. A monolith
might run as several separate Deployments or Rollouts in the same namespace —
one per role, each with its own scaling policy but the same codebase. Pods
can also be created and managed directly by a team's own controller, with no
Deployment or ReplicaSet above them at all.

Label targeting lets a single mirrord session target every pod that carries a
given Kubernetes label — instead of one workload name — so traffic from all
of them is intercepted together, however many separate workloads (or none)
actually own them.

### When to use this

- Your codebase is deployed multiple times under different roles in the same namespace,
  sharing a common label.
- Pods are managed directly by your own controller, with no Deployment or ReplicaSet
  above them to target.
- You want traffic from all of these pods mirrored or stolen into one local process,
  without merging or manually picking between them.

## Configuration
Object form:
```json
{
  "target": {
    "path": {
      "labels": { "argocd.argoproj.io/instance": "monolith-web" }
    }
  }
}
```
Shorthand Equivalent:
```json
{ "target": "label/argocd.argoproj.io/instance=monolith-web" }
```

Multiple labels are comma-separated in the string form (all must match):
```json
{ "target": "label/app=web,tier=frontend" }
```
An optional container:
```json
{ "target": "label/app=web,tier=frontend/container/app" }
```

### How it behaves

mirrord resolves the label selector against pods in the target namespace and:

- Mirrors or steals traffic from every matching pod.
- Resolves environment variables, file operations, and outgoing connections through a single pod that mirrord selects from the matches — not merged or repeated across every pod.

{% hint style="info" %}
This means label targeting fits best when every matching pod runs identical configuration and only differs in things like replica count or autoscaling policy. If your matching pods differ in environment variables or mounted files, pick a different targeting approach — or run one session per workload as before.
Minimum versions required are mirrord operator v3.192.0 and mirrord CLI v3.246.0.
{% endhint %}

### Limitations

- An empty label selector is rejected, so it can't accidentally match every pod in the namespace.
- Not yet supported: copy_target, queue splitting, DB branching, multi-cluster targeting, and preview environments. 
- Kubernetes' label selector syntax is not supported.
Combining label targeting with any of these currently returns an error rather than silently falling back to single-pod behavior.

### Not `mirrord up`

If what you need is to run several *different* applications together - not
variations of the same one - that's [`mirrord up`](multiple-concurrent-sessions.md)
instead. Label targeting intercepts traffic for pods that are all the *same*
application from a single session; `mirrord up` orchestrates *separate*
applications, each getting its own local process and its own target.
