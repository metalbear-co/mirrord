---
title: Profiles
date: 2024-03-25T00:00:00.000Z
lastmod: 2024-03-25T00:00:00.000Z
draft: false
images: []
linktitle: Profiles
menu: null
docs: null
teams: null
weight: 530
toc: true
tags:
  - team
  - enterprise
description: Reusable mirrord config templates
---

The installation of the mirrord operator defines two types of [custom resources](https://kubernetes.io/docs/concepts/extend-kubernetes/api-extension/custom-resources/) in your cluster: `MirrordClusterProfile` and `MirrordProfile`.

{% hint style="info" %}
This feature is available to users on the Team and Enterprise pricing plans.
{% endhint %}

These resources provide a unified base for users' mirrord configurations. Users can reference a profile in their mirrord config, and mirrord will apply the defined feature adjustments accordingly.

## Cluster-Wide Profiles (`MirrordClusterProfile`)

`MirrordClusterProfile` is a cluster-scoped custom resource that provides a shared base configuration available to all namespaces in the cluster.

```yaml
apiVersion: profiles.mirrord.metalbear.co/v1alpha
kind: MirrordClusterProfile
metadata:
  # This name can be referenced by the user in their mirrord configs.
  name: example-cluster-profile
spec:
  # A list of adjustments to be made in the user's feature config.
  #
  # The adjustments are applied in order.
  featureAdjustments:
    # Incoming traffic will be stolen.
    - change: incoming-steal
    # All outgoing traffic will be remote.
    - change: outgoing-remote
    # All DNS resolution will be remote.
    - change: dns-remote
```

## Namespaced Profiles (`MirrordProfile`)

`MirrordProfile` is a namespaced custom resource. These profiles are defined within a specific namespace and are only available to workloads running in that namespace.

```yaml
apiVersion: profiles.mirrord.metalbear.co/v1alpha
kind: MirrordProfile
metadata:
  name: example-profile
  namespace: example-namespace
spec:
  featureAdjustments:
    - change: incoming-steal
    - change: outgoing-remote
    - change: dns-remote
```

## Allowed Feature Adjustments

The complete list of allowed values for the `featureAdjustments.[].change` field is as follows:
1. `incoming-mirror` - incoming traffic will be mirrored
2. `incoming-steal` - incoming traffic will be stolen
3. `incoming-off` - incoming traffic will not be intercepted
4. `dns-remote` - all DNS resolution will be remote
5. `dns-off` - all DNS resolution will be local
6. `outgoing-remote` - all outgoing traffic will be remote
7. `outgoing-off` - all outgoing traffic will be local

## Selecting a profile

Starting from mirrord version 3.136.0, the user can select a mirrord profile in their mirrord config. The profile is referenced by its name.

```json
{
    "profile": "example-profile"
}
```

## Enforcing profiles

Use of mirrord profiles can be enforced with [mirrord policies](policies.md#profile-policy).

**Important:** mirrord profiles are applied to the session on the user machine, and should not be used as security features.
