---
title: mirrord Operator
description: "What the mirrord Operator does, what it needs from your cluster, and how to configure it for licensing, internal registries, and specific platforms."
tags:
  - team
  - enterprise
---

# mirrord Operator

The mirrord Operator is a Kubernetes operator that runs persistently in your cluster and manages mirrord sessions. It's the central component that enables all **\[Teams]** features.

## Why the Operator?

In the open-source version of mirrord, each session is standalone - mirrord injects itself into the local process and creates an agent pod directly. This works well for individual use, but doesn't support coordination between users.

The Operator solves this by acting as a centralized control plane:

- **Better security** - Users no longer need permissions to create privileged pods. Only the Operator does. Permissions are managed through Kubernetes RBAC.
- **Concurrent use** - The Operator coordinates multiple mirrord sessions on the same cluster, preventing conflicts.
- **Advanced features** - Support for [policies](../sharing-the-cluster/policies.md), [profiles](../sharing-the-cluster/profiles.md), [queue splitting](../sharing-the-cluster/queue-splitting.md), [DB branching](../sharing-the-cluster/db-branching.md), and more.

![mirrord for Teams - Architecture](../.gitbook/assets/operator-architecture.svg)

## Installation

To install the Operator, see [Install the Operator](../getting-started/install-the-operator.md).

## Requirements

Before you install the Operator, make sure that your cluster and network meet these requirements.

**Kubernetes version**

The Operator needs Kubernetes `1.22` or later. Some features need a newer version, listed in [Which Kubernetes versions does mirrord support?](../faq/limitations.md#which-kubernetes-versions-does-mirrord-support)

**Permissions**

Installing the Operator creates cluster-scoped objects: CRDs, ClusterRoles, an APIService, and a PriorityClass for agent pods. The user who installs it needs permission to create these objects. In most clusters, this is the `cluster-admin` role.

**Nodes**

- The Operator and agent images are available for `linux/amd64` and `linux/arm64`.
- The nodes where agents run need Linux kernel 5.4 or later and a containerd, CRI-O, or Docker container runtime. Most features work with older kernels, but traffic stealing can be unreliable before 5.4. An agent for a target runs on the same node as the target pod. A targetless agent can run on any node.

**Network**

- **API server to Operator:** The Kubernetes API server sends mirrord requests to the Operator through the APIService. The API server must be able to reach the Operator pods over TCP, on the port in the `operator.port` Helm value (443 by default). Private clusters often need a firewall rule for this. For an example, see [this GKE issue](../troubleshooting/common-issues.md#mirrord-operator-status-fails-with-503-service-unavailable-on-gke).
- **Operator to MetalBear:** A cloud-connected Operator sends HTTPS requests to `app.metalbear.com` (license) and `analytics.metalbear.com` (telemetry). Team and Free licenses require telemetry. If the Operator cannot reach the telemetry endpoint for more than one hour, new sessions are blocked. For Enterprise licenses, telemetry is optional. For more details, see [Licensing](licensing.md). If your cluster cannot send outbound traffic, you need an Enterprise license certificate (`license.pem`). Give it to the Operator directly (see [Air-gapped / offline clusters](#air-gapped-offline-clusters-enterprise)), or install it in an [Enterprise License Server](license-server.md) that your Operators connect to.
- **Image pulls:** Your nodes pull the mirrord images from `ghcr.io/metalbear-co`. Some features use other public images, for example the database images for DB branching. If your nodes cannot reach these registries, see [Using an Internal Registry](#using-an-internal-registry-optional).

**Resources**

The Operator runs as one pod by default. Agents run only while sessions use them. For the default CPU and memory values, see [Scalability](scalability.md).

## Rotating the cloud API key

Rotate and revoke the key at [app.metalbear.com](https://app.metalbear.com). When you revoke, you can choose a grace window so the current key keeps working while you roll the Operator over to the new one.

## Air-gapped / offline clusters (Enterprise)

Air-gapped or offline clusters can't reach the cloud to exchange an API key for a license, so Enterprise deployments in that situation use an offline **license certificate** instead. First add the Helm repository and download `values.yaml`, as in [Get the chart](../getting-started/install-the-operator.md#get-the-chart). Then, instead of a cloud API key, give the chart your certificate. If you have a `license.pem` file, set `license.file.secret.data.license.pem` in `values.yaml` using a YAML literal block:

```yaml
license:
  file:
    secret:
      data:
        license.pem: |
          -----BEGIN CERTIFICATE-----
          <contents of your license.pem file>
          -----END CERTIFICATE-----
```

Alternatively, reference a Kubernetes secret via `license.pemRef` in `values.yaml`:

```yaml
license:
  pemRef: mirrord-operator-license-pem
```

This only points the chart at a secret name, it doesn't require the secret to exist yet. Install the chart first (this also creates the `mirrord` namespace), then create the secret in it:

```bash
helm install -f values.yaml mirrord-operator metalbear/mirrord-operator

kubectl create secret generic mirrord-operator-license-pem \
  --namespace mirrord \
  --from-file=license.pem=/path/to/license.pem
```

The Operator pod will wait until the secret exists and start automatically once it's created, no restart needed. If you create the secret before installing instead, make sure the `mirrord` namespace already exists and is Helm-managed, otherwise `helm install` will fail to adopt it.

For a fully self-hosted setup, see the [license server](license-server.md).

## License key

{% hint style="warning" %}
**⚠️ Deprecated for cloud authentication**

For clusters that reach the mirrord cloud, the license key is being replaced by the [cloud API key](../getting-started/install-the-operator.md#cloud-api-key), which is now the default way the Operator authenticates and obtains its license. Existing cloud license-key installations keep working, but new ones should use a cloud API key.

This does not apply to the [license server](license-server.md): if you run your own license server, the license key is still the shared secret the Operator uses to authenticate to it (a value you choose, not a mirrord-issued credential) and remains required.
{% endhint %}

Set `license.key` to your key, then install:

```bash
helm install -f values.yaml mirrord-operator metalbear/mirrord-operator
```

Alternatively, reference a Kubernetes secret via `license.keyRef` in `values.yaml`:

```yaml
license:
  keyRef: mirrord-operator-license
```

This only points the chart at a secret name, it doesn't require the secret to exist yet. Install the chart first (this also creates the `mirrord` namespace), then create the secret in it:

```bash
helm install -f values.yaml mirrord-operator metalbear/mirrord-operator

kubectl create secret generic mirrord-operator-license \
  --namespace mirrord \
  --from-literal=OPERATOR_LICENSE_KEY=<your license key>
```

The Operator pod will wait until the secret exists and start automatically once it's created, no restart needed. If you create the secret before installing instead, make sure the `mirrord` namespace already exists and is Helm-managed, otherwise `helm install` will fail to adopt it.

## Using an Internal Registry (Optional)

Using an internal registry reduces startup time, ingress costs, and removes dependency on GitHub's registry.

### Feature-specific images

These images are only pulled when the corresponding feature is enabled:

| Image                   | Default                                     | Tag              | Description                                                                                   | Override                                                                  |
| ----------------------- | ------------------------------------------- | ---------------- | --------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------- |
| Kafka splitting sidecar | `ghcr.io/metalbear-co/operator-kafka-proxy` | Same as operator | JVM sidecar for Kafka splitting (only when `operator.kafkaSplittingSidecar.enabled` is true). | `operator.kafkaSplittingSidecar.image`                                    |
| MSSQL tools             | `ghcr.io/metalbear-co/mssql-tools`          | Same as operator | Init container and sidecar for MSSQL DB branching (provides `sqlpackage` and `sqlcmd`). Table data is exported with the `bcp` in the branch's SQL Server image. | Env `MSSQL_TOOLS_IMAGE` via `operator.extraEnv`                           |
| Flyway                  | `flyway/flyway`                             | `12`             | Flyway migration runner for DB branching.                                                     | Per-branch `migrations.image`, or `dbPod.migrationImages.flyway.registry` |
| Liquibase               | `liquibase/liquibase`                       | `4.33`           | Liquibase migration runner for DB branching.                                                  | Per-branch `migrations.image`, or `dbPod.migrationImages.liquibase.registry` |

### Code sync

A preview pod with [code sync](../use-cases/preview-environments-code-sync.md) runs the operator's image in an init container, and pulls it with the target pod's pull secrets. So when the operator image comes from a private registry, the target's pods must list a pull secret for that registry.

### DB branching default database images

DB branch pods pull a database image matching the engine. These are the defaults when no custom image is specified in the branch config:

| Engine      | Default image                                      | Override                                           |
| ----------- | -------------------------------------------------- | -------------------------------------------------- |
| PostgreSQL  | `docker.io/library/postgres:{version}`             | `operator.pgBranchConfig` - `dbPod.image`          |
| MySQL       | `docker.io/library/mysql:{version}`                | `operator.mysqlBranchConfig` - `dbPod.image`       |
| MariaDB     | `docker.io/library/mariadb:{version}`              | `operator.mariadbBranchConfig` - `dbPod.image`     |
| MongoDB     | `docker.io/library/mongo:{version}`                | `operator.mongodbBranchConfig` - `dbPod.image`     |
| MSSQL       | `mcr.microsoft.com/mssql/server:{version}`         | `operator.mssqlBranchConfig` - `dbPod.image`       |
| Redis       | `docker.io/library/redis:{version}`                | `operator.redisBranchConfig` - `dbPod.image`       |
| DynamoDB    | `amazon/dynamodb-local:{version}`                  | `operator.dynamodbBranchConfig` - `dbPod.image`    |
| ClickHouse  | `docker.io/clickhouse/clickhouse-server:{version}` | `operator.clickhouseBranchConfig` - `dbPod.image`  |
| CockroachDB | `docker.io/cockroachdb/cockroach:{version}`        | `operator.cockroachdbBranchConfig` - `dbPod.image` |
| Spanner     | `gcr.io/cloud-spanner-emulator/emulator:{version}` | `operator.spannerBranchConfig` - `dbPod.image`     |

[Generic branches](../sharing-the-cluster/db-branching/generic.md) have no default image - the user supplies the full image reference per branch. Admins can restrict which images are allowed with the `allowedImages` glob list under `operator.genericBranchConfig` - `dbPod.allowedImages` (when absent, all images are allowed), and `imagePullSecrets` in the same config covers private registries.

### DB branching migration Job environment

Container-flavor migration Jobs inherit the target container's `env` and `envFrom` by default, with the declared connection variables redirected to the branch. `dbPod.migrationEnv` on every `<db>BranchConfig` controls this: `inherit: false` opts out, and `env`/`envFrom` add admin-supplied values in plain Kubernetes shapes (so `valueFrom`/`secretRef` keep secret values in-cluster). See [Schema Migrations](../sharing-the-cluster/db-branching/migrations.md#admin-control-over-the-jobs-environment) for the full behavior and precedence.

### Copying images

We recommend [regctl](https://regclient.org/) for copying multi-arch images:

```sh
# Get the operator image version
IMAGE_VERSION=$(helm show chart metalbear/mirrord-operator | grep 'appVersion:' | awk '{print $2}')

# Copy operator image
regctl image copy ghcr.io/metalbear-co/operator:$IMAGE_VERSION your-registry/operator:$IMAGE_VERSION

# Get and copy agent image
AGENT_IMAGE_VERSION=$(regctl image config ghcr.io/metalbear-co/operator:$IMAGE_VERSION | jq -r '.config.Labels."metalbear.mirrord.version"')
regctl image copy ghcr.io/metalbear-co/mirrord:$AGENT_IMAGE_VERSION your-registry/mirrord:$AGENT_IMAGE_VERSION
```

Then set in your `values.yaml`:

```yaml
operator:
  image: your-registry/operator
agent:
  image:
    registry: your-registry/mirrord
```

### OpenShift

Apply the following SecurityContextConstraints:

```yaml
kind: SecurityContextConstraints
apiVersion: security.openshift.io/v1
metadata:
  name: scc-mirrord
allowHostPID: true
allowPrivilegedContainer: false
allowHostDirVolumePlugin: true
allowedCapabilities: ["SYS_ADMIN", "SYS_PTRACE", "NET_RAW", "NET_ADMIN"]
runAsUser:
  type: RunAsAny
seLinuxContext:
  type: MustRunAs
users:
  - system:serviceaccount:mirrord:mirrord-operator
  - system:serviceaccount:mirrord:default
```

### GKE Autopilot

In GKE Autopilot the mirrord Operator can be run as a [customer-owned privileged workload](https://docs.cloud.google.com/kubernetes-engine/docs/concepts/about-autopilot-privileged-workloads#customer-owned-privileged-workloads).

mirrord is an approved [GKE Autopilot partner](https://docs.cloud.google.com/kubernetes-engine/docs/resources/autopilot-partners). Because of this, you should **not** manually apply a [WorkloadAllowlist](https://docs.cloud.google.com/kubernetes-engine/docs/how-to/autopilot-privileged-allowlists) for the mirrord-agent workload: GKE Autopilot clusters reject direct manual installation of it with an admission error. Manual installation only worked previously in specially-configured test projects, not in standard customer clusters.

Instead, apply the following [AllowlistSynchronizer](https://docs.cloud.google.com/kubernetes-engine/docs/reference/crds/allowlistsynchronizer), which automatically syncs the current and future approved versions of the mirrord-agent allowlist:

```yaml
apiVersion: auto.gke.io/v1
kind: AllowlistSynchronizer
metadata:
  name: mirrord-allowlist
spec:
  allowlistPaths:
    - "mirrord/mirrord-agent/*"
```

**Note:** some Operator configurations might produce mirrord-agent pods that don't match this specification. When that happens, you'll see agent spawn errors in the Operator logs. To get the correct WorkloadAllowlist embedded in those error messages, merge this snippet into your mirrord Operator `values.yaml`:

```yaml
agent:
  annotations:
    cloud.google.com/generate-allowlist: "true"
```
