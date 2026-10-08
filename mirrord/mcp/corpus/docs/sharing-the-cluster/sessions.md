---
title: "Managing Sessions"
description: "View and manage active mirrord sessions in the cluster"
date: 2024-03-04T00:00:00+00:00
lastmod: 2024-03-04T00:00:00+00:00
draft: false
images: []
menu:
  docs:
    parent: "using-mirrord"
weight: 170
toc: true
tags: ["team", "enterprise"]
---
Whenever a user starts mirrord on a cluster where mirrord for Teams is installed, the Operator assigns a
session to this user, until they stop running mirrord, at which point the session is closed
in the Operator automatically.

{% hint style="info" %}
This feature is available to users on the Team and Enterprise pricing plans.
{% endhint %}

## See active Operator sessions

Users can use the command `mirrord operator status` to see active sessions in the cluster.
For example, in the following output, we can see the session ID, the target used, 
the namespace of the target, the session duration, and the user running that session. 
We can also see that `Ports` is empty, meaning the user isn't stealing or mirroring any 
traffic at the moment.

```
+------------------+-----------------------------+-----------+---------------------------------------------------------------+-------+------------------+
| Session ID       | Target                      | Namespace | User                                                          | Ports | Session Duration |
+------------------+-----------------------------+-----------+---------------------------------------------------------------+-------+------------------+
| 487F4F2B6D2376AD | deployment/ip-visit-counter | default   | Aviram Hassan/aviram@metalbear.co@avirams-macbook-pro-2.local |       | 4s               |
+------------------+-----------------------------+-----------+---------------------------------------------------------------+-------+------------------+
```

The `User` field is generated in the following format - `whoami/k8s-user@hostname`. 
`whoami` and `hostname` are from the local machine, while `k8s-user` is the user we see 
from the operator side.


In this example, we can see that the session has an active steal on port 80,
filtering HTTP traffic with the following filter: `baggage: .*mirrord-session=Avi.+`

```
+------------------+-----------------------------+-----------+---------------------------------------------------------------+----------------------------------------------------------+------------------+
| Session ID       | Target                      | Namespace | User                                                          | Ports                                                    | Session Duration |
+------------------+-----------------------------+-----------+---------------------------------------------------------------+----------------------------------------------------------+------------------+
| C527FE7D9C30979E | deployment/ip-visit-counter | default   | Aviram Hassan/aviram@metalbear.co@avirams-macbook-pro-2.local | Port: 80, Type: steal, Filter: header=baggage: .*mirrord-session=Avi.+ | 13s              |
+------------------+-----------------------------+-----------+---------------------------------------------------------------+----------------------------------------------------------+------------------+
```

## How long sessions live

{% hint style="info" %}
Configuring session lifetimes requires mirrord operator `3.191.0` or later and operator Helm chart `3.191.0` or later.
{% endhint %}

The Operator closes sessions on its own once nobody is using them, so a crashed client or a
cancelled CI job does not leave agents and patched workloads behind. The defaults suit most
clusters. This is useful when yours behaves differently, for example when CI runs against a
cluster that is slow to roll pods.

A session goes through two phases, and a different value applies to each.

| Phase | Lasts from | Until | Closed after |
| --- | --- | --- | --- |
| Starting up | the session is created | the first client connects | `sessionSetupDeadlineSeconds` |
| Connected | the first client connects | the client goes away | `sessionUnusedTtlSeconds` |

While a session is **starting up**, no client has reached it yet. The Operator may be patching
the target for queue splitting and waiting for its pods to roll, which takes time. The setup
deadline bounds this phase, so the session survives setup but is still cleaned up if a client
never arrives.

Once a client is **connected**, the session stays alive as long as the client is there. Activity
is recorded every few seconds, so the unused TTL counts from the moment the client goes away, not
from the last request.

The maximum session time is a separate cap that applies in both phases. It is the only value that
closes a session someone is actively using, and it is unset by default.

```yaml
operator:
  ## How long a session may wait for its first client connection. Default: 180.
  sessionSetupDeadlineSeconds: 180

  ## How long a connected session may go unused before it is closed. Default: 30.
  sessionUnusedTtlSeconds: 30

  ## Cap on total session lifetime, regardless of activity. Unset by default.
  maxSessionTimeSeconds: 3600
```

Each value covers its own phase, so none of them overrides another. A session is closed by
whichever one it reaches first.

### Silence on a live connection

{% hint style="info" %}
`communicationTimeoutMillis` requires operator Helm chart `3.210.0` or later. The Operator applies
the timeout either way, but earlier charts give you no way to change it.
{% endhint %}

The Operator times the client and the agent separately, and closes the session as soon as
**either** of them goes `communicationTimeoutMillis` without sending anything.

```yaml
operator:
  ## How long either side may go silent before the session is closed. Default: 60000.
  communicationTimeoutMillis: 60000
```

### Multi-cluster sessions

A multi-cluster session is tracked twice: one session per cluster, and one session on the primary
that owns them. Both go through the same two phases, so there is a matching pair of values under
`operator.multiCluster`.

```yaml
operator:
  multiCluster:
    ## How long the session may wait for its first client connection. Default: 180.
    sessionSetupDeadlineSeconds: 180

    ## How long it may go unused once a client has connected. Default: 60.
    sessionTtlSeconds: 60
```

Earlier chart versions named these `sessionTtlSecs` and `remoteSessionTimeoutSecs`. Both names
still work, so an existing values file needs no changes.

Change `multiCluster.sessionSetupDeadlineSeconds` and `sessionSetupDeadlineSeconds` together. The primary creates
a session on every cluster before a client connects to any of them, so multi-cluster sessions
spend longer starting up than single-cluster ones. Closing the session on the primary also closes
it on every other cluster.

### Picking values

| Symptom | Value to raise |
| --- | --- |
| Sessions fail while starting, with `Session not found` or `is being deleted` | `sessionSetupDeadlineSeconds` |
| Sessions drop for clients on slow or unreliable networks | `sessionUnusedTtlSeconds` |
| Sessions drop mid-use, with `Layer Communication Timeout` in the Operator log | `communicationTimeoutMillis` |

A longer unused TTL also holds agents and patched workloads for longer after a client really does
go away, so raise it only as far as you need.

Three values work best within these ranges:

| Value | Range | Reason |
| --- | --- | --- |
| `sessionUnusedTtlSeconds` | 6 or more | Activity is recorded every 3 seconds, so a shorter TTL can lapse between two updates while the client is still connected. The Operator raises anything lower to 6. |
| `sessionSetupDeadlineSeconds` | Above 150 with queue splitting | Queue splitting waits up to 150 seconds for the target's pods to become ready. |
| `maxSessionTimeSeconds` | Above `sessionSetupDeadlineSeconds` | A cap below the setup deadline reaches sessions before a client can connect to them. The Operator logs a warning on startup. |

The unused TTL and the setup deadline cover separate phases, so they can be set independently.

### Why a session was closed

The Operator records a reason whenever it closes a session, and mirrord reports it to the user.

| Reason | Meaning |
| --- | --- |
| `SetupDeadlineExceeded` | No client connected within `sessionSetupDeadlineSeconds` |
| `Unused` | The client disconnected and did not return within `sessionUnusedTtlSeconds` |
| `TimeLimitExceeded` | The session reached `maxSessionTimeSeconds` |
| `StateLost` | The session was created by an Operator instance that has since been replaced |

## Stop active Operator sessions

Users may also forcefully stop a session with the `mirrord operator session` CLI commands.
These allow users to manually close Operator sessions while they're still alive  (user is
still running mirrord).

The session management commands are:

- `mirrord operator session kill-all` which will forcefully stop **ALL** sessions!
- `mirrord operator session kill --id {id}` which will forcefully stop a session with `id`,
  where you may obtain the session id through `mirrord operator status`;

### `sessions` RBAC

Every `mirrord-operator-user` has access to **all** session operations by **default**, as it comes
with `deletecollection` and `delete` privileges for the `sessions` resource. The CI-specific
`mirrord-operator-ci` ClusterRole, intended for machine sessions such as `mirrord ci` and
`mirrord preview`, has the same session permissions by default. You may limit either role by
changing the RBAC configuration. Here is a sample `role.yaml` with the other Operator rules
omitted:

```yaml
apiVersion: rbac.authorization.k8s.io/v1
kind: ClusterRole
metadata:
  name: mirrord-operator-user
rules:
- apiGroups:
  - operator.metalbear.co
  resources:
  - sessions
  verbs:
  - deletecollection
  - delete
```

For CI identities, apply the same change to `mirrord-operator-ci`.

- `mirrord operator session kill-all` requires the `deletecollection` verb;
- `mirrord operator session kill --id {id}` requires the `delete` verb;
