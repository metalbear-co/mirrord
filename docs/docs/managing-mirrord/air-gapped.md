---
title: Air-gapped Clusters
date: 2026-11-11T00:00:00.000Z
lastmod: 2026-11-11T00:00:00.000Z
draft: false
images: []
linktitle: Air-gapped Clusters
menu:
  docs:
    teams: null
weight: 545
toc: true
tags:
  - enterprise
description: "Run the mirrord Operator in a cluster with no access to the internet or to external networks."
---

# Air-gapped Clusters

On the Enterprise plan, you can run the mirrord Operator in an air-gapped cluster, one with no access to the internet or to external networks, usually for security or compliance reasons. It gets its license from a certificate file or from your own [License Server](license-server.md), and you can turn off usage telemetry if your security policy requires it.

{% hint style="info" %}
This feature is available to users on the Enterprise pricing plan.
{% endhint %}

## License certificate or License Server

A certificate is enough for a single cluster. Use the License Server when you run mirrord in several clusters and want one seat count and a dashboard.

|  | License certificate | License Server |
|---|---|---|
| **Where the license lives** | A `license.pem` secret in each cluster | Your License Server, which every Operator connects to |
| **Seats** | Each Operator pod counts seats on its own | Counted centrally, across clusters |
| **Dashboard** | None | [License Server dashboard](dashboard/license-server.md) |
| **If it's unreachable** | Nothing to reach | Operators refuse new sessions after their hourly license check fails |

## Copy the chart and images

Your cluster can't reach our chart repository or image registries. On a machine with internet access, pull the chart:

```bash
helm repo add metalbear https://metalbear-co.github.io/charts
helm pull metalbear/mirrord-operator
```

Or pull it from the OCI registry:

```bash
helm pull oci://ghcr.io/metalbear-co/mirrord-operator
```

Push the chart to your internal chart repository, or copy the `.tgz` file. For a License Server, also pull its chart:

```bash
helm pull metalbear/mirrord-operator-license-server
```

Then copy the Operator and agent images, as in [Copying images](operator.md#copying-images). Some features also use third-party images, such as the databases for database branching. For the full list, see [Feature-specific images](operator.md#feature-specific-images).

## Set up the Operator with a license certificate

In `values.yaml`, point the chart at the images in your registry, as in [Copying images](operator.md#copying-images).

[Usage telemetry](security.md#what-data-does-the-mirrord-operator-send-to-metalbear-cloud) is on by default. You can turn it off:

```yaml
operator:
  disableTelemetries: true
```

Then add your certificate in one of two ways.

### In values.yaml

Add your certificate:

```yaml
license:
  file:
    data:
      license.pem: |
        -----BEGIN CERTIFICATE-----
        <contents of your license.pem file>
        -----END CERTIFICATE-----
```

Install the chart you copied:

```bash
helm install -f values.yaml mirrord-operator <your-chart-repo>/mirrord-operator
```

### As a Kubernetes secret

Reference a Kubernetes secret via `license.pemRef` in `values.yaml`:

```yaml
license:
  pemRef: mirrord-operator-license-pem
```

This only points the chart at a secret name, it doesn't require the secret to exist yet. Install the chart first (this also creates the `mirrord` namespace), then create the secret in it:

```bash
helm install -f values.yaml mirrord-operator <your-chart-repo>/mirrord-operator

kubectl create secret generic mirrord-operator-license-pem \
  --namespace mirrord \
  --from-file=license.pem=/path/to/license.pem
```

The Operator pod will wait until the secret exists and start automatically once it's created, no restart needed. If you create the secret before installing instead, make sure the `mirrord` namespace already exists and is Helm-managed, otherwise `helm install` will fail to adopt it.

### With a License Server

Follow [License Server](license-server.md). You can turn off telemetry on its Operators the same way.

## Renew the certificate

Replace the certificate before it expires. The Operator picks up the new one on its own, no restart needed.

If the certificate is in `values.yaml`, replace it there and upgrade:

```bash
helm upgrade -f values.yaml mirrord-operator <your-chart-repo>/mirrord-operator
```

If you use `license.pemRef`, replace the secret:

```bash
kubectl create secret generic mirrord-operator-license-pem \
  --namespace mirrord \
  --from-file=license.pem=/path/to/new/license.pem \
  --dry-run=client -o yaml | kubectl apply -f -
```
