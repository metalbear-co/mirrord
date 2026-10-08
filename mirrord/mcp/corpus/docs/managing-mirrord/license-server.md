---
title: License Server
date: 2025-04-07T00:00:00.000Z
lastmod: 2025-04-07T00:00:00.000Z
draft: false
images: []
linktitle: License Server
menu:
  docs:
    teams: null
weight: 540
toc: true
tags:
  - enterprise
description: License Server
---

The license server enables you to manage your organization’s seats without sending any data to mirrord’s servers. It can aggregate license metrics from multiple operators (useful if you’re running mirrord across multiple clusters) and provides visibility into seat usage across your organization.

{% hint style="info" %}
This feature is available to users on the Enterprise pricing plan.
{% endhint %}

#### Architecture

The license server exposes an API that the operator uses to obtain a license, which the operator requires to run. It also allows the operator to keep track of active users, check and enforce seat counts, and store telemetry data.

Authentication between the operator and license server is done using a license key (chosen by the person setting up the license server) and is not to be confused with the license certificate file provided by MetalBear.

#### Basic Setup

The license server is installable via Helm. First, add the MetalBear Helm repository:

```bash
helm repo add metalbear https://metalbear-co.github.io/charts
```

Next, save the following yaml as `values.yaml` on your machine.

```yaml
# ./values.yaml
createNamespace: true

service:
  type: ClusterIP

license:
  key: secret
  file:
    data:
      license.pem: |
        ----- ...
        MIRRORD-LICENSE
        ... -----
```

Fill in the license.key and license.pem fields according to the following guidelines:

* License key - Can be any string of your choosing. We recommend using random characters or a UUID.
* License file - Must be a valid operator license. This can also be a secret under the `license.pem` key.

You can customize the license server deployment further - all values.yaml configuration options can found [here](https://raw.githubusercontent.com/metalbear-co/charts/main/mirrord-license-server/values.yaml) (or, see [Using a Cluster Secret](license-server.md#using-a-cluster-secret) and [Using Google Secrets Manager](license-server.md#using-google-secrets-manager) below).

_NOTE: The license server needs to be accessible to any mirrord operators you want to track. To that end, the default value for `service.type` is `ClusterIP`, but can be changed to `NodePort` or `LoadBalancer`, according to your requirements._

Next, install the license server on your cluster:

```bash
helm install metalbear/mirrord-operator-license-server -f ./values.yaml --wait
```

To make sure it's been installed successfully and is running:

```bash
kubectl get deployment -n mirrord mirrord-license-server
```

If your operator(s) are running in a different cluster, make sure the `mirrord-operator-license-server` service is exposed to them via ingress.

##### Using a Cluster Secret

You can set the license key in a cluster secret within the operator's namespace (`mirrord` by default), and reference it in the license server helm chart via `license.keyRef`. For example, with the following `values.yaml`:

```yaml
# ./values.yaml
createNamespace: true

service:
  type: ClusterIP

license:
  keyRef: my-cluster-secret
  file:
    data:
      license.pem: |
        ----- ...
        MIRRORD-LICENSE
        ... -----
```

The secret itself, which must use the key `OPERATOR_LICENSE_KEY`, can be created like so:

```bash
kubectl create secret generic my-cluster-secret -n mirrord --from-literal OPERATOR_LICENSE_KEY=my-very-secret-string
```

##### Using Google Secrets Manager

You can fetch the license file from GSM by providing the secret path and service account credentials as follows:

```yaml
# ./values.yaml
createNamespace: true

service:
  type: ClusterIP

license:
  ## To access the secret, the license server will use Application Default Credentials.
  ## The easiest way to provide credentials is by allowing the license server's
  ## Kubernetes ServiceAccount to impersonate a GCP service account.
  ## This is done with the `.sa.gcpSa` setting below.
  gsmRef: "projects/<PROJECT_ID>/secrets/<SECRET_NAME>/versions/<SECRET_VERSION>"
  key: "my-very-secret-string"

sa:
  name: mirrord-operator-license-server
  ## GCP service account to impersonate.
  gcpSa: <IAM_SA_NAME>@<IAM_SA_PROJECT_ID>.iam.gserviceaccount.com
```

##### Connecting Operators to the License Server

First update your operator `values.yaml` file ([see this page](../getting-started/quick-start.md#helm) for quickstart helm setup for operator):

```yaml
# ./values.yaml
license:
  key: secret
  licenseServer: http://<license-server-addr>
```

_NOTE: The server value must contain the protocol and the prefix for any ingress that the the license server can be behind._

Then run:

```bash
helm install metalbear/mirrord-operator -f ./values.yaml --wait
```

### Getting a Utilisation Report from the License Server

{% hint style="info" %}
This feature requires at least mirrord-operator-license-server Helm chart version **1.4.0**.
{% endhint %}

The license server has an endpoint that can be used to get a spreadsheet report with stats about mirrord usage. It has general metrics as well as specific per-user metrics. The reports include:

* **General stats** for the timeframe specified in query parameters
  * Number of licenses acquired
  * Number of unique machines used OR unique kubernetes users, depending on the type of license being used
* **Per-user stats**
  * Oldest and most recent activity date
  * Total mirrord session time and number
  * Average (mean) session duration and average (mean) number of daily sessions over the timeframe specified in query parameters

**Query params**

* `format` (required):
  * `"xlsx"`: produces an Excel spreadsheet with two tabs: general stats and per-user stats (_As curl will warn you, this produces binary data, and should be directed into a file rather that printed out raw_).
* `from` (optional): A date time, e.g. `"2025-08-20T00:00:00Z"`, to bound the beginning of the period for relevant stats.
* `to` (optional): A date time, e.g. `"2025-08-20T00:00:00Z"`, to bound the end of the period for relevant stats.
* `primary_key` (optional, v3.130.0+): The primary key used to aggregate the data in the report. Can be one of: `machine_id` (default), `username` (the kubernetes username) , `client_username` (the user machine's username), `client_hostname` (the user machine's hostname)

{% hint style="info" %}
If the query parameters `to` and `from` are not specified, the data will be for all time.
{% endhint %}

To get a report:

* Ensure that the license server is accessible - by default it is installed as a `ClusterIP` so either expose it outside the cluster or access it from inside the cluster (for example, you can use `mirrord exec -- curl`).

* Run `curl` by referencing the license server service by its domain name and ensuring that you use the correct API license key, for example with the service name `mirrord-operator-license-server` in the namespace `mirrord` with cluster domain `cluster.local`:

```bash
mirrord exec -- curl "mirrord-operator-license-server.mirrord.svc.cluster.local/api/v1/reports/usage?format=xlsx" --header 'x-license-key: <operator API license key>' --output report.xlsx
```

### Getting Operator Failure Reports from the License Server

{% hint style="info" %}
This feature requires at least mirrord-operator-license-server Helm chart version **1.56.0**.
{% endhint %}

The license server has an endpoint that can be used to retrieve reports about errors collected from mirrord oeperator. These reports help administrators investigate issues affecting mirrord usage in their cluster.

Currently, these reports include operator system errors such as agent spawn failures.

The reports include:

* **Failure events** for the timeframe specified in query parameters
  * Timestamp of the failure event
  * Error message
  * Operator version
  * License hash
  * Instance ID

**Query params**

* `from` (optional): A date time, e.g. `"2025-08-20T00:00:00Z"`, to bound the beginning of the period for relevant failures.
* `to` (optional): A date time, e.g. `"2025-08-20T00:00:00Z"`, to bound the end of the period for relevant failures.

{% hint style="info" %}
If the query parameters `to` and `from` are not specified, the data will be for all available retained history.
{% endhint %}

**Retention**

Failure events are retained for a limited time.

* By default, operator error logs are kept for **30 days**

This can be configured via the Helm chart:

```yaml
server:
  retention:
    operatorErrors: 30
````

Expired error events are removed automatically by the license server based on the configured retention period.

To get a report:

* Ensure that the license server is accessible - by default it is installed as a `ClusterIP` so either expose it outside the cluster or access it from inside the cluster (for example, you can use `mirrord exec -- curl`).

* Run `curl` by referencing the license server service by its domain name and ensuring that you use the correct API license key, for example with the service name `mirrord-operator-license-server` in the namespace `mirrord` with cluster domain `cluster.local`:

```bash
mirrord exec -- curl "mirrord-operator-license-server.mirrord.svc.cluster.local/api/v1/reports/errors?from=2026-03-01T00:00:00Z&to=2026-03-02T00:00:00Z" --header 'x-license-key: <operator API license key>'
```

```json
{
  "entries": [
    {
      "timestamp": "2026-03-19T18:43:17Z",
      "error": "failed to spawn agent: ...",
      "operator_version": "3.130.0",
      "license_hash": "abc123...",
      "instance_id": "instance-1"
    }
  ]
}
```