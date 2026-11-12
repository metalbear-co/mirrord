---
title: Install the Operator
description: "Install the mirrord Operator in your cluster, with one command or with Helm."
tags:
  - team
  - enterprise
---

# Install the Operator

The [mirrord Operator](../managing-mirrord/operator.md) runs in your cluster and adds the Team and Enterprise features, such as [policies](../sharing-the-cluster/policies.md), [queue splitting](../sharing-the-cluster/queue-splitting.md) and [database branching](../sharing-the-cluster/db-branching.md) (see [pricing](https://metalbear.com/mirrord/pricing/) for the full list per plan). Install it once per cluster. From then on, mirrord uses it automatically for every session in that cluster.

If you install it for a trial, the Operator keeps working after the trial ends. Without a paid plan, it gives you the same features as open-source mirrord, plus a usage dashboard at [app.metalbear.com](https://app.metalbear.com) with the last 7 days of activity.

There are two ways to install it:

- **[Default install](#default-install):** one command, `mirrord operator install`, with the chart's default settings. Use it unless one of the Helm cases below applies.
- **[Custom install with Helm](#custom-install-with-helm):** if your platform team manages cluster software with Helm or GitOps, or you need to change a default. For example, your nodes can't pull images from `ghcr.io` (see [Using an Internal Registry](../managing-mirrord/operator.md#using-an-internal-registry-optional)), you want a namespace other than `mirrord`, or you run OpenShift or GKE Autopilot.

If your cluster is isolated from the internet and external networks, for example for security or compliance, see [Air-gapped / offline clusters](../managing-mirrord/operator.md#air-gapped-offline-clusters-enterprise) (Enterprise only).

## Requirements

- A mirrord for Teams license (Team or Enterprise) or a 7-day trial. To start the trial, sign up at [app.metalbear.com](https://app.metalbear.com), or run the [default install](#default-install) and sign up on the login page it opens.
- Kubernetes `1.22` or later. Some features need a newer version, listed in [Which Kubernetes versions does mirrord support?](../faq/limitations.md#which-kubernetes-versions-does-mirrord-support)
- Permission to create cluster-scoped objects, such as CRDs, ClusterRoles, and an APIService. In most clusters, that's the `cluster-admin` role. You need it only to install: the Operator itself runs with its own, narrower role (see [What can the Operator access in my cluster?](../managing-mirrord/security.md#what-can-the-operator-access-in-my-cluster)).
- Nodes running `linux/amd64` or `linux/arm64`, with Linux kernel 5.4 or later and a containerd, CRI-O, or Docker runtime.
- The Kubernetes API server can reach the Operator pods over TCP, on the port in the `operator.port` Helm value (443 by default). Private clusters often need a firewall rule for this. For an example, see [this GKE issue](../troubleshooting/common-issues.md#mirrord-operator-status-fails-with-503-service-unavailable-on-gke).
- Your nodes can pull the mirrord images from `ghcr.io/metalbear-co`. Some features pull other public images, such as the database images for DB branching. If your nodes can't reach these registries, see [Using an Internal Registry](../managing-mirrord/operator.md#using-an-internal-registry-optional).

For details on each requirement, see [Requirements](../managing-mirrord/operator.md#requirements) on the Operator page.

{% hint style="warning" %}
**Security:** the Operator runs inside your cluster and sends MetalBear only license checks and usage metrics. The metrics are anonymized unless identity sharing is on for your organization's API key. In a single cluster, developers can only use mirrord on workloads their own Kubernetes access already lets them see. Before you install, read [What can the Operator access in my cluster?](../managing-mirrord/security.md#what-can-the-operator-access-in-my-cluster), or share it with your security team.
{% endhint %}

## Default install

You need mirrord CLI `3.278.0` or later (check with `mirrord --version`) and a kubeconfig pointing at the cluster. Only an organization admin can run the first install. If your organization already has an operator API key, for example, from an earlier install, [install with that key](#install-with-an-existing-key) instead.

1. Run:

   ```bash
   mirrord operator install
   ```

   This installs into your current kubecontext, or into the `kube_context` in your [Global Configuration](../installing-mirrord/global-configuration.md) if you set one. To use a different one, add `--context <name>`.

2. If you aren't logged in to app.metalbear.com, a browser window opens. Approve the login there. If the browser doesn't open, open the URL that the command prints.

3. The command checks for an existing Operator and for your permissions, then asks:

   ```text
   ? Install the mirrord operator into namespace `mirrord` of kubecontext `my-cluster`? (y/N)
   ```

   Type `y` and press Enter. To skip the question, pass `--yes`. In CI, or anywhere without a terminal, the command doesn't ask.

4. Wait for the command to finish. It creates your organization's operator API key, installs the Operator, and gives it up to 5 minutes to become ready. At the end, you see output like this, with your own version and names:

   ```text
   ✓ mirrord operator install
     ✓ no operator installed
     ✓ fetching the operator manifest
     ✓ checking permissions
     ✓ creating the operator API key of the organization of you@example.com
     ✓ installing the operator
     ✓ waiting for the operator to become ready
   mirrord operator 3.230.0 is installed in namespace `mirrord` of kubecontext `my-cluster`. To remove it, run `mirrord operator uninstall --context my-cluster`.
   ```

5. [Verify the installation](#verify-the-installation).

{% hint style="info" %}
**AI coding agents:** to install without a person, follow [Agent-Started Trials](../using-mirrord-with-ai/agent-started-trials.md).
{% endhint %}

### Install with an existing key

Your organization has one operator API key, and the first default install creates it. Once the key exists, install every other cluster with it. Anyone who has the key can do this without logging in:

```bash
mirrord operator install --api-key <key>
```

To read the key from a cluster where you ran the default install:

```bash
kubectl get deployment mirrord-operator -n mirrord \
  -o jsonpath='{.spec.template.spec.containers[*].env[?(@.name=="OPERATOR_CLOUD_API_KEY")].value}'
```

For a Helm install, the key is in the secret or Secret Manager entry you gave the chart. Or ask an organization admin.

## Custom install with Helm

### Get the chart

Add the MetalBear Helm repository and download the chart's `values.yaml`:

```bash
helm repo add metalbear https://metalbear-co.github.io/charts
curl https://raw.githubusercontent.com/metalbear-co/charts/main/mirrord-operator/values.yaml --output values.yaml
```

Edit the values you need:

- To install into a namespace other than `mirrord`, set `namespace`. Then use your namespace instead of `mirrord` in the commands on this page.
- To pull images from your own registry, see [Using an Internal Registry](../managing-mirrord/operator.md#using-an-internal-registry-optional).
- On OpenShift or GKE Autopilot, see [OpenShift](../managing-mirrord/operator.md#openshift) or [GKE Autopilot](../managing-mirrord/operator.md#gke-autopilot).
- To let developers target [serverless workloads](../using-mirrord/serverless/README.md) outside Kubernetes, such as Amazon ECS tasks, set `operator.sessionsManager: true`. This is experimental. See [Operator-Hosted Sessions-Manager](../using-mirrord/serverless/operator-hosted.md).

### Cloud API key

The Operator uses a cloud API key to get its license. Generate one under **Settings** at [app.metalbear.com](https://app.metalbear.com). It's shown only once, so store it right away.

### Add the key to your values

Pick one:

**Kubernetes secret (recommended):** keeps the key out of your `values.yaml`. Set the secret's name now, and create the secret after installing:

```yaml
cloud:
  apiKey:
    keyRef: mirrord-operator-cloud-api-key
```

**Google Secret Manager:** store the key in GSM. The Operator reads it with Application Default Credentials (see `sa.gcpSa`):

```yaml
cloud:
  apiKey:
    gsmRef: projects/PROJECT_ID/secrets/SECRET_NAME/versions/latest
```

### Install the chart

```bash
helm install -f values.yaml mirrord-operator metalbear/mirrord-operator
```

If you chose a Kubernetes secret, create it in the new `mirrord` namespace:

```bash
kubectl create secret generic mirrord-operator-cloud-api-key \
  --namespace mirrord \
  --from-literal=apiKey=<your API key>
```

The Operator starts as soon as the secret exists, so you don't need to restart anything. Creating the secret before installing works only if the `mirrord` namespace already exists and is managed by Helm. Otherwise, `helm install` can't adopt the namespace.

Then [verify the installation](#verify-the-installation).

## Verify the installation

If the Operator isn't in your current kubecontext, add `--context <name>` to the `kubectl` command, and run `mirrord operator status` with `MIRRORD_KUBE_CONTEXT=<name>` set.

1. Check that the Operator pod is running:

   ```bash
   kubectl get pods -n mirrord
   ```

   ```text
   NAME                                READY   STATUS    RESTARTS   AGE
   mirrord-operator-7c9d8b6f5d-x2k4p   1/1     Running   0          1m
   ```

2. Check that mirrord can reach the Operator:

   ```bash
   mirrord operator status
   ```

   The output starts with the Operator version and your organization's license, for example:

   ```text
   Operator version: 3.230.0
   Operator default namespace: default
   Operator License
       name: Acme
       organization: Acme
       expire at: 31-Dec-2026
   ```

If either check fails, see [Common Issues](../troubleshooting/common-issues.md).

## Uninstall

- **Default install:** run `mirrord operator uninstall`. Add `--context <name>` for a different kubecontext. This also deletes all mirrord policies and profiles in the cluster. On a terminal, the command asks you to confirm first. With `--yes`, or without a terminal, it doesn't ask.
- **Helm:** run `helm uninstall mirrord-operator` with the same kubecontext and current namespace you had when you ran `helm install`. Helm keeps its release record there, not in the namespace the chart installs the Operator into.

## Next steps

- [Policies](../sharing-the-cluster/policies.md): control what developers can do in the cluster.
- [Sharing the Cluster](../sharing-the-cluster/overview.md): concurrent sessions, queue splitting, and database branching.
- [Onboarding Wizard](onboarding-wizard.md): guides developers through their first mirrord config.
