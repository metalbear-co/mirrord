---
title: Policies
date: 2024-01-03T13:37:00.000Z
lastmod: 2024-01-03T13:37:00.000Z
draft: false
images: []
linktitle: Policies
menu: null
docs: null
teams: null
weight: 520
toc: true
tags:
  - team
  - enterprise
description: Limiting available features for selected targets with mirrord for Teams
---

The installation of the mirrord operator defines two [custom resources](https://kubernetes.io/docs/concepts/extend-kubernetes/api-extension/custom-resources/) in your cluster: the namespaced `MirrordPolicy` and the cluster-wide `MirrordClusterPolicy`. With these policies you can limit the use of some features of mirrord for selected targets.

* `MirrordPolicy` and `MirrordClusterPolicy` have the exact same specification (`spec` field);
* `MirrordPolicy` applies only to targets living in the same namespace;
* `MirrordClusterPolicy` applies to all targets in the cluster.

{% hint style="info" %}
This feature is available to users on the Team and Enterprise pricing plans.
{% endhint %}

## Blockable features

Currently the set of blockable features contains:

* `steal` - prevents stealing traffic from the targeted pods;
* `steal-without-filter` - prevents stealing traffic from the targeted pods, unless HTTP filter is used;
* `mirror` - prevents mirroring traffic from the targeted pods.

If you are not using the latest operator version, the set of supported blockable features might be different. In order to see the exact set of features you can block, use the following `kubectl` command:

```shell
kubectl get crd mirrordpolicies.policies.mirrord.metalbear.co -o jsonpath='{.spec.versions[-1].schema.openAPIV3Schema.properties.spec.properties.block.items.enum}'
```

Here is an example of a policy that blocks stealing when no filter is defined:

```yaml
apiVersion: policies.mirrord.metalbear.co/v1alpha
kind: MirrordClusterPolicy
metadata:
  name: block-steal-without-filter
spec:
  targetPath: "*"
  block:
    - steal-without-filter
```




## Controllable features

Some policies are not for outright blocking features, instead they change behaviour, overriding what the user has set in their mirrord config file.

You may use these features to change which files may be accessed in the target, or which environment variables may be retrieved. These policies should not be relied upon for security, and should instead be considered convenience policies.

### env policy

_Added in mirrord Operator version 3.103.0_

Changes how environment variables may be retrieved from the target, overriding what the user has set in their `mirrord.json` config file.

* `exclude` - the environment variables in this list **WON'T** be retrieved from the target, instead mirrord will either use the locally available env vars (if they exist in the user's machine), or these env vars will be missing completely;

The policy takes priority over a user's mirrord config, which means that if the user has a config:

```json
{
  "feature": {
    "env": {
      "include": "*_URL"
    }
  }
}
```

If the policy is set with `exclude: ["*_URL"]`, then mirror will **NOT** retrieve env vars that match `*_URL`, even though the user explicitly wanted that in their config.

If you are not using the latest operator version, the env policy options might be different. In order to see the latest options, use the following `kubectl` command:

```shell
kubectl get crd mirrordpolicies.policies.mirrord.metalbear.co -o jsonpath='{.spec.versions[-1].schema.openAPIV3Schema.properties.spec.properties.env}'
```

### fs policy

_Added in mirrord Operator version 3.103.0_

Changes file operations behaviour, giving the operator control over which files may be accessed from the target, and in which modes. Overrides what the user has set in their `mirrord.json` config file.

* `readOnly` - files that match any of the patterns specified here must be opened as **read-only**, otherwise the operation will fail;
* `local` - matching files will be forced to be opened locally, on the user's machine, instead of in the target;
* `notFound` - any matching files will return a _not found_ error as if the file is not present in the target, even if it exists there;

The policy takes priority over a user's mirrord config, which means that if the user has a config:

```json
{
  "feature": {
    "fs": {
      "read_write": ".+\\.json"
    }
  }
}
```

If the policy is set with `readOnly: [".+\\.json"]`, and the user tries to open a file that matches this regex in _write_ mode, then mirrord will return an error to the user app, as if the file could not be found, even though the user wanted it to be `read_write`.

```shell
kubectl get crd mirrordpolicies.policies.mirrord.metalbear.co -o jsonpath='{.spec.versions[-1].schema.openAPIV3Schema.properties.spec.properties.fs}'
```

### network policy

_Added in mirrord Operator version 3.105.0_

Allows the operator to control which patterns may be used as [HTTP header filters](../using-mirrord/incoming-traffic/filter-incoming-traffic.md#filtering-a-subset-of-traffic-with-mirror-or-steal-mode). Header filters specified by the user must match the regex specified in the network policy.

```yaml
apiVersion: policies.mirrord.metalbear.co/v1alpha
kind: MirrordPolicy
metadata: { ... }
spec:
  ...
  network:
    incoming:
      httpFilter:
        headerFilter: "^baggage: .+"
```

If the policy is set with `headerFilter: "^baggage: .+"` at least one header filter must match the `^baggage: .+` regex when user is using the steal mode for incoming traffic.

```json
{
  "feature": {
    "network": {
      "incoming": {
        "http_filter": {
          "header_filter": "^baggage: .*mirrord-session=alice.*"
        }
      }
    }
  }
}
```

this also works _any of_ or _all of_ patterns

```json
{
  "feature": {
    "network": {
      "incoming": {
        "http_filter": {
          "all_of": [
            {
              "header": "^baggage: .*mirrord-session=alice.*"
            },
            {
              "path": "/api.*"
            }
          ]
        }
      }
    }
  }
}
```

```json
{
  "feature": {
    "network": {
      "incoming": {
        "http_filter": {
          "any_of": [
            {
              "header": "^baggage: .*mirrord-session=alice.*"
            },
            {
              "header": "^tracestate: .*mirrord-session=alice.*"
            }
          ]
        }
      }
    }
  }
}
```

**Important:** `steal-without-filter` will be automatically enabled once any http filter is specified.

### profile policy

_Added in mirrord Operator version 3.108.0_

Allows the operator to enforce using a [mirrord profile](profiles.md) and to specify a set of allowed profiles.

```yaml
apiVersion: policies.mirrord.metalbear.co/v1alpha
kind: MirrordPolicy
metadata: { ... }
spec:
  ...
  # If this is set, the user must select a mirrord profile for their session.
  #
  # If multiple policies apply to the given session,
  # a profile is required if at least one of them require it.
  #
  # Optional, defaults to false.
  requireProfile: true
  # A list of allowed mirrord profiles.
  #
  # If multiple policies apply to the given session,
  # user's selected profile must be present in all allowlists.
  #
  # Optional. If not present, this policy will not enforce any allowlist.
  profileAllowlist:
  - my-profile-1
  - my-profile-2
```

The example above will enforce that the user selects either `my-profile-1` or `my-profile-2` for their session.

**Important:** mirrord profiles are applied to the session on the user machine, and should not be used as security features.

### split queues policy

_Added in mirrord Operator version 3.199.0_

Allows the operator to require [queue splitting](queue-splitting.md) and to control which message filters users may split with. This is useful when a session that consumes queue messages without splitting would compete with the deployed workload (and with other users' split sessions) for messages.

The policy only takes effect on targets that have split queues configured (a `MirrordSplitConfig`, or a legacy `MirrordWorkloadQueueRegistry`/`MirrordKafkaTopicsConsumer`). A namespace-wide or cluster-wide policy does not block sessions against targets that consume no queues.

```yaml
apiVersion: policies.mirrord.metalbear.co/v1alpha
kind: MirrordPolicy
metadata: { ... }
spec:
  ...
  splitQueues:
    # When the target has split queues configured for some broker kind,
    # the session must split queues of that kind, otherwise it is rejected.
    #
    # Optional, defaults to false.
    requireFilter: true
    # Requirements checked against the message filters of the session's split queues.
    # Each entry applies to the split queues whose id matches the `queueId` regex
    # and whose broker kind is `queueType`.
    #
    # Optional.
    filters:
      - queueId: "^(ship-queue|order-queue)$"
        queueType: sqs
        rules:
          # The queue's message filter must satisfy every one of these rules.
          allOf:
            - key: user_id
              # Regex applied to the filter pattern the user set for this key.
              # The user's pattern is itself a regex; use `^exact$` to demand
              # one exact pattern. When unset, any pattern is accepted.
              pattern: '^\\d\+$'
            - key: company_id
      - queueId: "^click-topic$"
        queueType: kafka
        rules:
          # The queue's message filter must satisfy at least one of these rules.
          anyOf:
            - key: author
            - key: user_id
```

With `requireFilter: true`, a session against a target with configured SQS and Kafka queues must set `feature.split_queues` entries for both kinds. A split queue with no filter at all is still accepted - such a queue delivers no messages to the session, so it does not disturb anyone.

The `filters` rules only apply to queues the session actually filters. In the example above, a session splitting `order-queue` with:

```yaml
feature:
  split_queues:
    order-queue:
      queue_type: SQS
      message_filter:
        user_id: '\d+'
        company_id: "42"
```

passes, because the filter has both required keys and the `user_id` pattern `\d+` matches the policy's `^\\d\+$` regex. A filter missing `company_id`, or one with a different `user_id` pattern, is rejected. A queue covered by rules but filtered only with `jq_filter` is rejected too: the operator cannot verify what a jq program matches, so use `message_filter` (alone or alongside jq) on such queues. The composable `filter` shape is checked branch by branch: every `any_of` branch must satisfy the rule (any branch alone can let a message through), and one `all_of` branch is enough. A `metadata` regex cannot prove which attribute it filters on, so on a rule-covered queue it is rejected like a lone `jq_filter`; name the attribute with `message_filter` there.

If a `queueId` regex matches none of the session's split queues, the entry simply does not apply to that session.

**Note:** the policy is enforced at creation time - of the session, the copy target, or the preview environment.

## Restricting targets affected by mirrord policies

By default, mirrord policies apply to all targets in the namespace or cluster. You can use a target path pattern (`.spec.targetPath`) and/or a [label selector](https://kubernetes.io/docs/concepts/overview/working-with-objects/labels/#resources-that-support-set-based-requirements) (`.spec.selector`) in order to limit the targets to which a policy applies.

The target path of a mirrord run is either `targetless` or has the form `<TARGET_TYPE>/<NAME>` followed by an optional
`/container/<CONTAINER_NAME>`, where `<TARGET_TYPE>` is one of `deploy`, `pod`, `rollout`, `statefulset`, `replicaset`, `job`,
`cronjob`, `service`, or `label`.

Examples for possible target paths:

* `deploy/boats`
* `pod/boats-5fffb9767c-w92qh`
* `pod/boats-5fffb9767c-w92qh/container/appcontainer`
* `targetless`

By specifying a `targetPath` pattern in the policy, you limit the policy to only apply to runs that have a target path that matches the specified pattern.

The target path pattern can contain `?`, which will match a single character, and `*`, which will match arbitrarily many characters. For example, `"deploy/*"` will make a policy apply for any run with a deployment target. `"*boats*"` will make a policy apply to any target with `boats` in its resource name, e.g. `pod/boats-2kljw9`.

### Operator versions 3.200.0 and higher

#### Inheritance

Resources inherit policies from their [owning resources](https://kubernetes.io/docs/concepts/overview/working-with-objects/owners-dependents/) (and their owners, and so on). This means that a policy with target `deploy/my-deployment` will apply to `my-deployment`, the replica set it owns _and_ all pods owned by that replica set.

Policies do not work in the opposite direction, so a policy with target `pod/my-pod` will not apply to the owners of the pod.

An example of a policy:

```yaml
apiVersion: policies.mirrord.metalbear.co/v1alpha
kind: MirrordPolicy
metadata:
  name: block-unfiltered-stealing-from-deployments
  namespace: books
spec:
  targetPath: "deploy/*"
  block:
    - steal-without-filter
    - mirror
```

This policy blocks mirroring and unfiltered stealing of traffic coming to all deployments and all resources owned by deployments (replica sets, pods) in the namespace `books`.

##### Labels and inheritance

When both the target path and a label selector are specified, policies are inherited  only by dependents (such as pods) of resources that match both the path and the selector. This can lead to subtle differences in behaviour depending on how you write a policy.

Take this policy:

```yaml
apiVersion: policies.mirrord.metalbear.co/v1alpha
kind: MirrordPolicy
metadata:
  name: block-mirroring-from-blue-deployments
  namespace: default
spec:
  targetPath: "deploy/*"
  selector:
    matchLabels:
      colour: blue
  block:
    - mirror
```

This policy blocks mirroring of traffic coming to all deployments which are marked with label `colour: blue`, and their dependent resources. If a deployment `my-deployment` has the label `colour: blue`, the policy applies to all of its pods regardless of the pod's `color`.

However, if a deployment `your-deployment` has the label `colour: yellow`, and owns pods with the label `colour: blue`, this policy _will not apply to the blue pods_. To apply a policy to blue pods in `your-deployment`, you can instead use something like:

```yaml
...
spec:
  targetPath: "pod/your-deployment*"
  selector:
    matchLabels:
      colour: blue
...
```

#### Containers

A target path is considered in two parts: a resource path like `pod/boats-5fffb9767c-w92qh`, and an optional container path like `container/appcontainer`. Policies that have a target without a container specified will apply to all containers.

To apply a policy to specific containers, `/container/` _must_ be explicitly present **without wildcards** in the target path. The path `deploy/*abc` will match all containers on all deployments where the deployment name ends in `abc` (and their dependents), but will not match `deploy/my-deployment/container/abc`.

For example, in a policy:

```yaml
apiVersion: policies.mirrord.metalbear.co/v1alpha
kind: MirrordPolicy
metadata:
  name: block-stealing-from-lib-containers
  namespace: books
spec:
  targetPath: "*/container/lib*"
  block:
    - steal
```

This blocks stealing from any container name starting with `lib` in the namespace `books`, regardless of resource type.

{% hint style="warning" %}
Ensure the target path has wildcards if required. `/container/*` will match resources with an empty resource path, and therefore will match nothing. `*/container/*` is the correct syntax to target all resources.
{% endhint %}

### Operator versions lower than 3.200.0

Policies are applied according to the session target given to mirrord. It is possible for a policy to apply to a deployment target, but not to apply to the deployment's pods when targeted directly. For example, the following policy:

```yaml
apiVersion: policies.mirrord.metalbear.co/v1alpha
kind: MirrordPolicy
metadata:
  name: block-stealing-from-boats-deployment
  namespace: default
spec:
  targetPath: "deploy/boats*"
  block:
    - steal
```

prevents mirrord users from stealing traffic when using the whole `boats` deployment as a target. However, a user could still use a specific pod out of that deployment as a target for mirrord and steal its traffic. In order to prevent that, the `targetPath` pattern or the label selector needs to be changed to match the pods of that deployment.

#### Labels

If a workload is used as a target, this workload's labels will be used to match against policies' `selector`, if set. If a pod is used as a target, the pod's labels will be used.

#### Containers

> **Note**: Containers in target paths are not treated differently from other parts of the path.
> This means the pattern `deploy/my-deployment` will not match when a container is specified. That pattern can be changed to `deploy/my-deployment*` to also match on runs with a specified container (but will then also match `deploy/my-deployment-1` etc.)
