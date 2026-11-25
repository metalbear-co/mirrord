---
title: "Multi-Cluster Setup"
description: "Step-by-step guide to setting up multi-cluster mirrord: install the operator on every cluster, choose an authentication method, and connect them."
tags:
  - alpha
  - enterprise
---

# Multi-Cluster Setup

This guide covers how to set up multi-cluster mirrord. At a glance, the setup is:

1. Install the mirrord operator on every downstream cluster with `operator.multiClusterMember=true` ([Setting Up Downstream Clusters](#setting-up-downstream-clusters)).
2. Pick an [authentication method](#authentication-methods) for each downstream cluster and prepare its credentials.
3. Install the operator on the Primary cluster with `operator.multiCluster.enabled=true` and the `clusters` map ([Configuring the Primary Cluster](#configuring-the-primary-cluster)).
4. [Verify](#verify-the-connection) that the Primary can reach every downstream cluster.

## Prerequisites

Before you start, make sure you have:

1. The mirrord operator `3.141.0+` Helm chart `1.50.0+` ready to install on all clusters.
2. Kubernetes `1.30` or later on the Primary cluster, and `1.22` or later on the downstream clusters. The downstream clusters need `1.30` or later if you use [queue splitting](../sharing-the-cluster/queue-splitting.md).
3. `kubectl` access to all clusters.
4. For EKS IAM authentication: AWS CLI and `eksctl` installed.
5. For AKS Workload Identity authentication: Azure CLI (`az`) installed, and the Primary AKS cluster must have [Workload Identity enabled](https://learn.microsoft.com/en-us/azure/aks/workload-identity-deploy-cluster).
6. For GKE authentication: `gcloud` installed, and a Google identity for the Primary operator: a Google service account it acts as through [Workload Identity](https://cloud.google.com/kubernetes-engine/docs/how-to/workload-identity) when the Primary runs on GKE, or a [Workload Identity Federation](https://cloud.google.com/iam/docs/workload-identity-federation-with-kubernetes) principal when it runs elsewhere.

---

## Authentication Methods

For each downstream cluster, you must specify an `authType` that determines how the Primary mirrord operator authenticates to it.

### Bearer Token (`authType: bearerToken`)

Uses ServiceAccount tokens that are automatically refreshed via the Kubernetes TokenRequest API. Good for most setups where the Primary cluster can reach the downstream cluster's API server.

You generate an initial token manually during setup. After that, the operator auto-refreshes the token before it expires using the TokenRequest API. The refreshed token keeps the same lifetime as the original.

### EKS IAM (`authType: eks`)

For AWS EKS clusters. The Primary operator generates short-lived tokens using its IAM role (via IRSA). No secrets to manage - tokens are generated and refreshed automatically every 10 minutes.

On the Primary cluster, the operator pod gets AWS credentials through IRSA (`sa.roleArn`). It uses those credentials to generate a presigned STS URL, which is sent to the downstream cluster as a bearer token. The downstream EKS cluster validates the token with AWS STS, then maps the IAM role to a Kubernetes group via an [Access Entry](https://docs.aws.amazon.com/eks/latest/userguide/access-entries.html). Kubernetes RBAC on the downstream cluster grants permissions to that group.

No Kubernetes Secret is needed - authentication is entirely through IAM.

### AKS Workload Identity (`authType: aks`)

For Azure AKS clusters. The Primary operator generates tokens by exchanging its projected ServiceAccount token with Entra ID (formerly Azure AD). No secrets to manage - tokens are generated and refreshed automatically at the halfway point of the token's lifetime (~12 hours for a typical 24-hour Entra ID token).

On the Primary cluster, the operator pod gets Azure credentials through [Workload Identity](https://learn.microsoft.com/en-us/azure/aks/workload-identity-overview) (`sa.azureClientId`). The Workload Identity webhook injects a projected ServiceAccount token (SA token) file and environment variables into the pod. An SA token is a short-lived JWT that Kubernetes issues for a pod's ServiceAccount and refreshes automatically; here it acts as proof of the pod's identity. The operator exchanges this SA token with Entra ID for an access token scoped to the AKS API server. The downstream AKS cluster validates the token with Entra ID, then maps the identity to a Kubernetes group via Azure RBAC or a ClusterRoleBinding.

No Kubernetes Secret is needed - authentication is entirely through Entra ID.

### GKE (`authType: gke`)

For Google Kubernetes Engine clusters. Needs the operator and chart `3.235.0` or later on the Primary and on each GKE downstream cluster. The Primary operator sends a Google access token for its own Google identity, the same token `kubectl` sends through the `gke-gcloud-auth-plugin`. No secrets to manage - the operator mints a new token halfway through each one's lifetime. For a Google service account's token it reads that lifetime from Google's tokeninfo endpoint, so a GKE Primary must reach `oauth2.googleapis.com` as well as the downstream API servers. A federated principal's token cannot be introspected, so an AKS or EKS Primary treats each token as short-lived and mints every 3 minutes; it must also reach `sts.googleapis.com`, where its federated token is exchanged for the Google one, and with that host blocked no token is ever minted.

The token comes from the pod's [Application Default Credentials](https://cloud.google.com/docs/authentication/application-default-credentials). On a GKE Primary that is Workload Identity: `sa.gcpSa` binds the operator's ServiceAccount to a Google service account and the metadata server mints the token. On an AKS or EKS Primary it is Workload Identity Federation: a credentials file, named by `GOOGLE_APPLICATION_CREDENTIALS`, that exchanges the pod's projected ServiceAccount token at Google's STS. The downstream GKE cluster validates the token with Google and maps it to the Google service account's email, or to its `principal://` for a federated identity. A ClusterRoleBinding on the downstream cluster grants that identity permissions.

No Kubernetes Secret is needed - authentication is entirely through Google.

### mTLS (`authType: mtls`)

For clusters that require client certificate authentication. You provide the client certificate and key in the cluster configuration or Secret.

Kubernetes does not auto-refresh mTLS client certificates. You are responsible for rotating the certificates you provide before they expire.

### Fields per Auth Type

| Field | `bearerToken` | `eks` | `aks` | `gke` | `mtls` |
|-------|:---:|:---:|:---:|:---:|:---:|
| `server` | Required | Required | Required | Required | Required |
| `caData` | Optional | Optional | Required | IP endpoint only | Optional |
| `bearerToken` | Required (initial only, auto-refreshed after) | - | - | - | - |
| `region` | - | Required | - | - | - |
| `tenantId` | - | - | Optional | - | - |
| `clientId` | - | - | Optional | - | - |
| `tlsCrt` | - | - | - | - | Required |
| `tlsKey` | - | - | - | - | Required |

---

## Setting Up Downstream Clusters

Every downstream cluster needs the mirrord operator installed with the `operator.multiClusterMember` helm chart value set to `true`. This creates the `ServiceAccount`, `ClusterRoles`, and `ClusterRoleBindings` that the Primary operator needs to manage sessions on that cluster.

### Bearer Token / mTLS Clusters

{% stepper %}
{% step %}

#### Install the operator on the downstream cluster

```bash
helm install mirrord-operator metalbear/mirrord-operator \
  --set operator.multiClusterMember=true
```

This creates a `mirrord-operator-envoy` ServiceAccount with the necessary RBAC permissions.

{% endstep %}
{% step %}

#### Generate an initial token (bearer token auth only)

```bash
kubectl create token mirrord-operator-envoy -n mirrord --duration=24h
```

Save this token - you'll need it when configuring the Primary cluster. This initial token is only needed once during setup. After the Primary operator starts, it auto-refreshes tokens using the TokenRequest API before they expire.

For mTLS, skip this step. Instead, you'll provide the client certificate and key when configuring the Primary cluster.

{% endstep %}
{% endstepper %}

### EKS IAM Clusters

EKS IAM authentication lets the Primary operator authenticate to downstream EKS clusters using its AWS IAM role. No Kubernetes Secrets to manage — the operator generates short-lived tokens from its IAM identity.

#### How EKS IAM Authentication Works

The Primary operator pod needs to talk to downstream EKS clusters. To do that, it needs a token. Here's how the token gets created and accepted:

1. **The pod gets AWS credentials** — the `sa.roleArn` annotation on the Primary operator's ServiceAccount tells EKS to inject AWS credentials into the pod via IRSA. Now the pod can act as that IAM role.

2. **The pod creates a token** — the operator uses those AWS credentials to generate a presigned `GetCallerIdentity` STS URL. This URL is used as a Kubernetes bearer token that EKS understands.

3. **The downstream cluster validates the token** — when the downstream EKS cluster receives this token, it calls AWS STS to verify the identity. AWS responds: "This is IAM role X."

4. **EKS maps the IAM role to a Kubernetes group** — the Access Entry on the downstream cluster maps that IAM role to a Kubernetes group (e.g. `mirrord-operator-envoy`). Now the operator is authenticated as a member of that group.

5. **Kubernetes RBAC grants permissions** — the ClusterRoleBindings on the downstream cluster (created by Helm with `multiClusterMemberIamGroup`) grant the `mirrord-operator-envoy` group the necessary permissions.

#### What Goes Where

| Component | Where | Purpose |
|-----------|-------|---------|
| OIDC Identity Provider | Primary cluster (AWS) | Enables IRSA so the pod can assume an IAM role |
| IAM role + trust policy | AWS IAM | The identity the operator pod assumes. Has no AWS permissions — only used as a Kubernetes identity |
| `sa.roleArn` in Helm | Primary cluster | Annotates the operator's ServiceAccount so the pod gets AWS credentials for the IAM role |
| Access Entry | Each downstream EKS cluster (AWS) | Maps the IAM role to a Kubernetes group. Created via `aws eks create-access-entry` |
| `multiClusterMemberIamGroup` in Helm | Each downstream cluster | Creates ClusterRoleBindings that grant permissions to the Kubernetes group |

{% hint style="info" %}
The Primary cluster does **not** need an Access Entry. The operator pod runs inside the Primary cluster, so it authenticates using its ServiceAccount — no IAM token needed. The Access Entries are only needed on downstream clusters where the pod authenticates from the outside.
{% endhint %}

#### Setup Steps

{% stepper %}
{% step %}

#### Associate an OIDC Identity Provider with the Primary cluster

This is a one-time AWS setup. It registers the Primary EKS cluster's OIDC issuer as an IAM Identity Provider, which is what enables IRSA — the mechanism that lets pods assume IAM roles.

```bash
eksctl utils associate-iam-oidc-provider \
  --cluster <PRIMARY_CLUSTER_NAME> --region <REGION> --approve
```

Skip this if already done (e.g. if other workloads in the cluster already use IRSA).

{% endstep %}
{% step %}

#### Create the IAM role with a trust policy

Create an IAM role that the operator pod will assume. The trust policy controls who can assume this role — it should only allow the Primary operator's ServiceAccount.

```json
{
  "Version": "2012-10-17",
  "Statement": [
    {
      "Effect": "Allow",
      "Principal": {
        "Federated": "arn:aws:iam::<ACCOUNT_ID>:oidc-provider/oidc.eks.<REGION>.amazonaws.com/id/<OIDC_ISSUER_ID>"
      },
      "Action": "sts:AssumeRoleWithWebIdentity",
      "Condition": {
        "StringEquals": {
          "oidc.eks.<REGION>.amazonaws.com/id/<OIDC_ISSUER_ID>:sub": "system:serviceaccount:mirrord:mirrord-operator",
          "oidc.eks.<REGION>.amazonaws.com/id/<OIDC_ISSUER_ID>:aud": "sts.amazonaws.com"
        }
      }
    }
  ]
}
```

You can find the OIDC issuer ID from the Primary cluster:

```bash
aws eks describe-cluster --name <PRIMARY_CLUSTER_NAME> --region <REGION> \
  --query "cluster.identity.oidc.issuer" --output text
# Returns: https://oidc.eks.<REGION>.amazonaws.com/id/<OIDC_ISSUER_ID>
```

{% hint style="info" %}
This IAM role does **not** need any IAM policies attached (no `s3:*`, `sqs:*`, etc.). It has zero AWS permissions. All actual permissions come from Kubernetes RBAC on the downstream clusters. The role is only used as an identity.
{% endhint %}

{% endstep %}
{% step %}

#### Create an EKS Access Entry on each downstream cluster

This is an AWS-level configuration (not a Kubernetes resource). It tells each downstream EKS cluster: "When this IAM role authenticates, map it to the `mirrord-operator-envoy` Kubernetes group."

Run this for **each downstream EKS cluster**:

```bash
aws eks create-access-entry \
  --cluster-name <DOWNSTREAM_CLUSTER_NAME> \
  --principal-arn arn:aws:iam::<ACCOUNT_ID>:role/<IAM_ROLE_NAME> \
  --type STANDARD \
  --kubernetes-groups mirrord-operator-envoy
```

The Access Entry itself doesn't grant any Kubernetes permissions — it only establishes the identity mapping. Permissions come from RBAC (next step).

{% endstep %}
{% step %}

#### Install the operator on each downstream cluster

Install the operator with `multiClusterMember` and `multiClusterMemberIamGroup`:

```bash
helm install mirrord-operator metalbear/mirrord-operator \
  --set operator.multiClusterMember=true \
  --set operator.multiClusterMemberIamGroup=mirrord-operator-envoy
```

This creates ClusterRoleBindings that grant the `mirrord-operator-envoy` Kubernetes group the permissions needed to manage sessions. The Access Entry (previous step) maps the IAM role to this group, so the Primary operator gets these permissions when it authenticates.

{% endstep %}
{% step %}

#### Install the operator on the Primary cluster with `sa.roleArn`

On the Primary cluster, set `sa.roleArn` so the operator pod can assume the IAM role:

```yaml
sa:
  roleArn: "arn:aws:iam::<ACCOUNT_ID>:role/<IAM_ROLE_NAME>"
```

This annotates the operator's ServiceAccount with `eks.amazonaws.com/role-arn`. When the pod starts, EKS injects AWS credentials into the pod automatically. The operator uses these credentials to generate tokens for each downstream cluster.

See the [Configuring the Primary Cluster](#configuring-the-primary-cluster) section below for the full Helm values.

{% endstep %}
{% endstepper %}

### AKS Workload Identity Clusters

AKS Workload Identity lets the Primary operator authenticate to downstream AKS clusters using its Azure Managed Identity. No Kubernetes Secrets to manage - the operator exchanges its projected SA token with Entra ID for access tokens.

#### How AKS Workload Identity Authentication Works

The Primary operator pod needs to talk to downstream AKS clusters. To do that, it needs a token. Here's how the token gets created and accepted:

1. **The pod gets Azure credentials** - the `sa.azureClientId` annotation on the Primary operator's ServiceAccount tells the Workload Identity webhook to inject Azure credentials (env vars + a projected SA token file) into the pod. The SA token itself is minted and refreshed by the Primary cluster's kubelet for the operator's ServiceAccount, so it is always fresh and needs no manual management.

2. **The pod creates a token** - the operator reads the projected SA token from the file and sends it to Entra ID, asking for an access token scoped to the AKS API server (app ID `6dae42f8-4368-4678-94ff-3960e28e3630`).

3. **Entra ID validates and issues a token** - Entra ID checks the SA token's signature against the Primary cluster's OIDC issuer, verifies it matches a Federated Identity Credential, and returns an access token (~24 hour lifetime).

4. **The downstream cluster validates the token** - when the downstream AKS cluster receives this token, it validates it with Entra ID. Entra ID confirms the identity.

5. **Kubernetes RBAC grants permissions** - the ClusterRoleBindings on the downstream cluster (created by Helm with `multiClusterMemberAzureGroup`) grant the Entra ID group the necessary permissions.

#### What Goes Where

| Component | Where | Purpose |
|-----------|-------|---------|
| Workload Identity + OIDC issuer | Primary AKS cluster | Enables the pod to get Azure credentials via a projected SA token |
| User-Assigned Managed Identity | Entra ID | The identity the operator pod uses. Has no Azure permissions - only used as a Kubernetes identity |
| Federated Identity Credential | Entra ID | Links the Primary cluster's SA token to the Managed Identity |
| `sa.azureClientId` in Helm | Primary cluster | Annotates the operator's ServiceAccount so the webhook injects Azure credentials |
| Azure RBAC role assignment or K8s RBAC | Each downstream AKS cluster | Grants the Managed Identity access to the cluster |
| `multiClusterMemberAzureGroup` in Helm | Each downstream cluster | Creates ClusterRoleBindings that grant permissions to the Entra ID group |

{% hint style="info" %}
The Primary cluster does **not** need special RBAC for itself. The operator pod runs inside the Primary cluster, so it authenticates using its ServiceAccount - no Entra ID token needed. The Federated Identity Credential and role assignments are only needed for downstream clusters where the pod authenticates from the outside.
{% endhint %}

#### Setup Steps

{% stepper %}
{% step %}

#### Enable Workload Identity on the Primary AKS cluster

If not already enabled:

```bash
az aks update \
  --name <PRIMARY_CLUSTER_NAME> \
  --resource-group <RESOURCE_GROUP> \
  --enable-oidc-issuer \
  --enable-workload-identity
```

{% endstep %}
{% step %}

#### Create a User-Assigned Managed Identity

```bash
az identity create \
  --name mirrord-operator-mc \
  --resource-group <RESOURCE_GROUP> \
  --location <LOCATION>
```

Note the `clientId` from the output - you'll need it for `sa.azureClientId`.

{% endstep %}
{% step %}

#### Create a Federated Identity Credential

This tells Entra ID: "when a token comes from the Primary cluster's OIDC issuer, signed for the `mirrord-operator` ServiceAccount, trust it as this Managed Identity."

```bash
# Get the Primary cluster's OIDC issuer URL
OIDC_ISSUER=$(az aks show \
  --name <PRIMARY_CLUSTER_NAME> \
  --resource-group <RESOURCE_GROUP> \
  --query "oidcIssuerProfile.issuerUrl" -o tsv)

# Create the federation
az identity federated-credential create \
  --name mirrord-operator-federation \
  --identity-name mirrord-operator-mc \
  --resource-group <RESOURCE_GROUP> \
  --issuer "$OIDC_ISSUER" \
  --subject "system:serviceaccount:mirrord:mirrord-operator" \
  --audiences "api://AzureADTokenExchange"
```

{% hint style="info" %}
The `--subject` must match the operator's ServiceAccount namespace and name exactly. If you use a custom namespace or SA name, adjust accordingly.
{% endhint %}

{% endstep %}
{% step %}

#### Enable Entra ID on each downstream AKS cluster

Each downstream AKS cluster must have Entra ID integration enabled so it can validate Entra ID tokens from the Managed Identity:

```bash
az aks update \
  --name <DOWNSTREAM_CLUSTER_NAME> \
  --resource-group <RESOURCE_GROUP> \
  --enable-aad
```

{% hint style="warning" %}
If you skip this step, the downstream cluster's API server won't understand Entra ID tokens and will return `401 Unauthorized`.
{% endhint %}

{% endstep %}
{% step %}

#### Grant access on each downstream AKS cluster

The Managed Identity needs Kubernetes-level permissions on each downstream cluster to manage sessions. Bind the Managed Identity's `principalId` (object ID) directly to the ClusterRoles created by Helm:

```bash
IDENTITY_OID=$(az identity show \
  --name mirrord-operator-mc \
  --resource-group <RESOURCE_GROUP> \
  --query principalId -o tsv)

kubectl --context <DOWNSTREAM_CLUSTER> create clusterrolebinding mirrord-operator-envoy-identity \
  --clusterrole=mirrord-operator-envoy \
  --user="$IDENTITY_OID"

kubectl --context <DOWNSTREAM_CLUSTER> create clusterrolebinding mirrord-operator-envoy-remote-identity \
  --clusterrole=mirrord-operator-envoy-remote \
  --user="$IDENTITY_OID"
```

{% hint style="info" %}
The `mirrord-operator-envoy` and `mirrord-operator-envoy-remote` ClusterRoles are created by Helm when you install with `multiClusterMember=true`.
{% endhint %}

{% endstep %}
{% step %}

#### Install the operator on each downstream cluster

Install the operator with `multiClusterMember` and `multiClusterMemberAzureGroup`:

```bash
helm install mirrord-operator metalbear/mirrord-operator \
  --set operator.multiClusterMember=true \
  --set operator.multiClusterMemberAzureGroup=mirrord-operator-envoy
```

This creates ClusterRoleBindings that grant the `mirrord-operator-envoy` group the permissions needed to manage sessions.

{% endstep %}
{% step %}

#### Install the operator on the Primary cluster with `sa.azureClientId`

On the Primary cluster, set `sa.azureClientId` so the Workload Identity webhook injects Azure credentials into the pod. You also need the downstream cluster's CA certificate (`caData`) since AKS clusters use per-cluster self-signed CAs:

```bash
# Get the downstream cluster's CA certificate (base64 encoded)
TMPKUBE=$(mktemp)
az aks get-credentials --name <DOWNSTREAM_CLUSTER> --resource-group <RESOURCE_GROUP> \
  --admin --file "$TMPKUBE"
grep certificate-authority-data "$TMPKUBE" | awk '{print $2}'
rm -f "$TMPKUBE"
```

```yaml
sa:
  azureClientId: "<MANAGED_IDENTITY_CLIENT_ID>"
```

This annotates the operator's ServiceAccount with `azure.workload.identity/client-id` and adds the `azure.workload.identity/use: "true"` pod label. When the pod starts, the webhook injects the projected SA token file and Azure env vars automatically.

See the [Configuring the Primary Cluster](#configuring-the-primary-cluster) section below for the full Helm values.

{% endstep %}
{% endstepper %}

### GKE Clusters

GKE authentication lets the Primary operator reach downstream GKE clusters as a Google identity. No Kubernetes Secrets to manage - the operator reads a Google access token from its Application Default Credentials.

#### How GKE Authentication Works

1. **The pod gets Google credentials** - on a GKE Primary, `sa.gcpSa` annotates the operator's ServiceAccount with `iam.gke.io/gcp-service-account`, and the GKE metadata server mints tokens for that Google service account. (Without the annotation it mints tokens for the Kubernetes ServiceAccount itself, but Google accepts those only at the API server of the pod's own cluster, so a downstream cluster needs the Google service account.) On an AKS or EKS Primary, a Workload Identity Federation credentials file (`GOOGLE_APPLICATION_CREDENTIALS`) points at the pod's projected ServiceAccount token, and the Google client library exchanges it at Google's STS.

2. **The pod mints a token** - the operator asks the Google client library for an access token with the `cloud-platform` and `userinfo.email` scopes and sends it as the bearer token. Google access tokens live one hour; the operator mints a new one halfway to expiry when Google reports the lifetime (a Google service account's token), and every 3 minutes when it does not (a federated principal's token, see [Token Refresh](#token-refresh)).

3. **The downstream cluster validates the token** - the GKE API server checks the token with Google and resolves it to the identity: the Google service account's email, or the `principal://` of a federated identity from outside Google Cloud.

4. **IAM lets the identity in, Kubernetes RBAC grants permissions** - GKE checks `container.clusters.get` in IAM before it reads Kubernetes RBAC, so the identity needs `roles/container.clusterViewer` on the downstream cluster's project. Everything else comes from the ClusterRoleBindings on the downstream cluster (created by Helm with `multiClusterMemberGcpPrincipal`), which grant that identity the operator's permissions.

#### What Goes Where

| Component | Where | Purpose |
|-----------|-------|---------|
| Google identity | Google Cloud IAM | What the operator pod acts as: a Google service account on a GKE Primary (reuse the one `sa.gcpSa` already names if the Primary has one), the Workload Identity Federation `principal://` elsewhere |
| `roles/container.clusterViewer` | Each downstream cluster's project | Lets GKE accept the identity (`container.clusters.get`); Kubernetes RBAC decides the rest |
| Workload Identity binding (`roles/iam.workloadIdentityUser`) | Primary GKE cluster's project | Lets the operator's ServiceAccount act as the Google service account |
| `sa.gcpSa` in Helm | Primary cluster | Annotates the operator's ServiceAccount so the metadata server mints tokens for the Google service account |
| Workload Identity Federation pool, provider and credentials file | Only for a Primary outside Google Cloud | Trusts the Primary cluster's OIDC issuer and exchanges the pod's token at Google's STS |
| `multiClusterMemberGcpPrincipal` in Helm | Each downstream cluster | Creates ClusterRoleBindings that grant permissions to the Google identity |

#### Setup Steps

{% stepper %}
{% step %}

#### Give the Primary operator a Google identity

On a GKE Primary with [Workload Identity enabled](https://cloud.google.com/kubernetes-engine/docs/how-to/workload-identity), the identity is a Google service account. `sa.gcpSa` is the operator's one Google identity: if the Primary already sets it, for license secrets in Google Secret Manager, `cloud.apiKey.gsmRef` or Pub/Sub splitting, use that account in every step here instead of creating a new one, or those features lose their roles. Otherwise create one and let the operator's ServiceAccount act as it:

```bash
gcloud iam service-accounts create mirrord-operator-envoy \
  --project <PRIMARY_PROJECT>

gcloud iam service-accounts add-iam-policy-binding \
  mirrord-operator-envoy@<PRIMARY_PROJECT>.iam.gserviceaccount.com \
  --role roles/iam.workloadIdentityUser \
  --member "serviceAccount:<PRIMARY_PROJECT>.svc.id.goog[mirrord/mirrord-operator]"
```

The member names the Primary cluster's Workload Identity pool and the operator's namespace and ServiceAccount (`mirrord/mirrord-operator` by default). The Google service account is not optional on a GKE Primary: without it the metadata server mints tokens for the Kubernetes ServiceAccount itself, and Google accepts those only at the API server of the pod's own cluster ([its federated identity table](https://cloud.google.com/iam/docs/federated-identity-supported-services) asks for service account impersonation towards any other GKE cluster).

On an AKS or EKS Primary there is no Google service account: the identity is the federated principal itself. Create a [Workload Identity Federation pool and provider](https://cloud.google.com/iam/docs/workload-identity-federation-with-kubernetes) that trusts the Primary cluster's OIDC issuer, then generate the credentials file without `--service-account`, so the token belongs to the principal (with that flag the token would belong to the service account, which would then need `roles/iam.workloadIdentityUser` for the principal, and GKE would refuse a token bound to the principal alone):

```bash
gcloud iam workload-identity-pools create-cred-config \
  projects/<PROJECT_NUMBER>/locations/global/workloadIdentityPools/<POOL>/providers/<PROVIDER> \
  --credential-source-file /var/run/service-account/token \
  --credential-source-type text \
  --output-file credentials.json
```

Put the file in a ConfigMap and mount it with the projected token through the chart's `operator.extraEnv`, `operator.extraVolumes` and `operator.extraVolumeMounts`; the audience of the projected token is the provider's full resource name:

```yaml
operator:
  extraEnv:
    GOOGLE_APPLICATION_CREDENTIALS: /var/run/google/credentials.json
  extraVolumes:
    - name: google-credentials
      configMap:
        name: mirrord-operator-google-credentials
    - name: google-token
      projected:
        sources:
          - serviceAccountToken:
              path: token
              expirationSeconds: 3600
              audience: //iam.googleapis.com/projects/<PROJECT_NUMBER>/locations/global/workloadIdentityPools/<POOL>/providers/<PROVIDER>
  extraVolumeMounts:
    - name: google-credentials
      mountPath: /var/run/google
      readOnly: true
    - name: google-token
      mountPath: /var/run/service-account
      readOnly: true
```

The identity the downstream cluster sees is then `principal://iam.googleapis.com/projects/<PROJECT_NUMBER>/locations/global/workloadIdentityPools/<POOL>/subject/system:serviceaccount:mirrord:mirrord-operator`, and it takes the place of the service account email in the steps below.

{% endstep %}
{% step %}

#### Install the operator on each downstream cluster

Install the operator with `multiClusterMember` and `multiClusterMemberGcpPrincipal`, set to the identity as the downstream cluster's RBAC names it: the Google service account's email, or the `principal://` of a federated identity:

```bash
helm install mirrord-operator metalbear/mirrord-operator \
  --set operator.multiClusterMember=true \
  --set operator.multiClusterMemberGcpPrincipal=mirrord-operator-envoy@<PRIMARY_PROJECT>.iam.gserviceaccount.com
```

This creates the `mirrord-operator-envoy` and `mirrord-operator-envoy-remote` ClusterRoles and binds them to that identity as a `User`. The downstream cluster may be in any project, but GKE checks IAM before Kubernetes RBAC, so grant the identity `roles/container.clusterViewer` there:

```bash
gcloud projects add-iam-policy-binding <DOWNSTREAM_PROJECT> \
  --member "serviceAccount:mirrord-operator-envoy@<PRIMARY_PROJECT>.iam.gserviceaccount.com" \
  --role roles/container.clusterViewer
```

For a federated identity, the member is its `principal://` instead.

{% endstep %}
{% step %}

#### Install the operator on the Primary cluster with `sa.gcpSa`

On a GKE Primary, set `sa.gcpSa` so the operator's ServiceAccount is bound to the Google service account from step 1 (an AKS or EKS Primary has the credentials file from step 1 instead and sets nothing here). You also need each downstream cluster's endpoint. A cluster reached at its IP endpoint serves a certificate from its own CA, so pass that CA as `caData`; a cluster reached at its DNS endpoint (`<id>.<region>.gke.goog`, the only endpoint when IP endpoints are turned off) serves a publicly trusted certificate, so leave `caData` out there, since with `caData` set the operator trusts that CA alone:

```bash
# IP endpoint and its CA
gcloud container clusters describe <DOWNSTREAM_CLUSTER> --location <LOCATION> \
  --project <DOWNSTREAM_PROJECT> --format 'value(endpoint)'
gcloud container clusters describe <DOWNSTREAM_CLUSTER> --location <LOCATION> \
  --project <DOWNSTREAM_PROJECT> --format 'value(masterAuth.clusterCaCertificate)'
# DNS endpoint, no caData
gcloud container clusters describe <DOWNSTREAM_CLUSTER> --location <LOCATION> \
  --project <DOWNSTREAM_PROJECT> --format 'value(controlPlaneEndpointsConfig.dnsEndpointConfig.endpoint)'
```

```yaml
sa:
  gcpSa: "mirrord-operator-envoy@<PRIMARY_PROJECT>.iam.gserviceaccount.com"
```

The chart adds the `iam.gke.io/gcp-service-account` annotation only to a ServiceAccount it creates. With `sa.create: false`, add that annotation to your existing ServiceAccount yourself.

See the [Configuring the Primary Cluster](#configuring-the-primary-cluster) section below for the full Helm values.

{% endstep %}
{% endstepper %}

---

## Configuring the Primary Cluster

Install the operator on the Primary cluster with multi-cluster enabled and all downstream clusters configured.

### Helm Values

```yaml
operator:
  multiCluster:
    # Enable multi-cluster mode
    enabled: true

    # Logical name of this cluster (set by you, should match the real cluster name)
    clusterName: "eu-west-1"

    # Default cluster for stateful operations (env vars, files, db branching)
    defaultCluster: "us-east-1"

    # Set to true if the Primary cluster has no workloads
    managementOnly: false

    # Downstream cluster configuration
    # Each key should match the real cluster name
    clusters:
      # Bearer token authentication
      us-east-1:
        authType: bearerToken
        server: "https://api.cluster-a.example.com:6443"
        caData: "LS0tLS1CRUdJTi..."
        bearerToken: "eyJhbGciOiJS..."
        # Namespace where the mirrord operator runs on this cluster (default: mirrord)
        # namespace: mirrord

      # EKS IAM authentication
      us-west-1:
        authType: eks
        region: eu-north-1
        server: "https://ABCDEF1234567890.gr7.eu-north-1.eks.amazonaws.com"
        caData: "LS0tLS1CRUdJTi..."

      # AKS Workload Identity authentication
      aks-eastus:
        authType: aks
        server: "https://aks-eastus-xxxxx.hcp.eastus.azmk8s.io:443"
        caData: "LS0tLS1CRUdJTi..."
        # tenantId and clientId are optional - defaults to env vars from webhook
        # tenantId: "xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx"
        # clientId: "xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx"

      # GKE authentication; caData only for an IP endpoint, none for a DNS endpoint
      gke-us-central1:
        authType: gke
        server: "https://34.0.0.1"
        caData: "LS0tLS1CRUdJTi..."

      # mTLS authentication
      us-north-1:
        authType: mtls
        server: "https://api.cluster-c.example.com:6443"
        caData: "LS0tLS1CRUdJTi..."
        tlsCrt: "LS0tLS1CRUdJTi..."
        tlsKey: "LS0tLS1CRUdJTi..."

# Required for EKS IAM auth - gives the pod AWS credentials
sa:
  roleArn: "arn:aws:iam::<ACCOUNT_ID>:role/<IAM_ROLE_NAME>"

# Required for AKS Workload Identity auth - triggers webhook injection
# sa:
#   azureClientId: "<MANAGED_IDENTITY_CLIENT_ID>"

# Required for GKE auth from a GKE Primary - binds the pod to a Google service account
# sa:
#   gcpSa: "mirrord-operator-envoy@<PRIMARY_PROJECT>.iam.gserviceaccount.com"
```

{% hint style="warning" %}
The cluster key names in the `clusters` map should match the real cluster names. For EKS clusters this is especially important — the operator uses the key as the EKS cluster name when signing IAM tokens.
{% endhint %}

### Where Data Is Stored

When you provide cluster configuration in the Helm values, the chart splits it into two places. Non-sensitive configuration (`server`, `caData`, `authType`, `region`, `namespace`) goes into the ConfigMap (`clusters-config.yaml`). `namespace` is the namespace where the mirrord operator runs on that cluster (default `mirrord`), not a workload namespace. The Default cluster is set only by `defaultCluster`. The per-cluster `isDefault` field is deprecated and ignored. Use `defaultCluster`. Sensitive credentials (`bearerToken`, `tls.crt`, `tls.key`) go into a Secret (`mirrord-cluster-<name>`).

For EKS IAM, AKS Workload Identity and GKE clusters, no Secret is created - everything is in the ConfigMap since authentication is through IAM, Entra ID or the operator's Google identity, not stored credentials.

### Manual Secret Creation

If you prefer to manage secrets outside of Helm values, you can create the Secret manually. The Secret must be labeled with `operator.metalbear.co/remote-cluster-credentials=true` and named `mirrord-cluster-<cluster-name>`. The cluster configuration (server, authType, etc.) still needs to be in the Helm values or the `clusters-config.yaml` ConfigMap.

For bearer token:

```yaml
apiVersion: v1
kind: Secret
metadata:
  name: mirrord-cluster-cluster-a
  namespace: mirrord
  labels:
    operator.metalbear.co/remote-cluster-credentials: "true"
type: Opaque
stringData:
  bearerToken: "eyJhbGciOiJS..."
```

For mTLS:

```yaml
apiVersion: v1
kind: Secret
metadata:
  name: mirrord-cluster-cluster-c
  namespace: mirrord
  labels:
    operator.metalbear.co/remote-cluster-credentials: "true"
type: Opaque
stringData:
  tls.crt: "LS0tLS1CRUdJTi..."
  tls.key: "LS0tLS1CRUdJTi..."
```

{% hint style="info" %}
EKS IAM, AKS Workload Identity and GKE clusters do not need a Secret at all. They authenticate using the operator's IAM role, Azure identity or Google identity and only need the cluster configuration in the ConfigMap.
{% endhint %}

---

## Preview Environment Replicas

To run previews as replicas on every workload cluster (see [Preview Environments in Multi-Cluster](multi-cluster.md#preview-environments-in-multi-cluster)), you need the operator and chart `3.193.0` or later on every cluster, and mirrord `3.247.0` or later. Set the mode on **every** operator - the Primary and all workload clusters:

```yaml
operator:
  multiCluster:
    preview:
      mode: replicas
```

For previews that use database branching:

* **Each workload cluster needs Kubernetes `1.29` or later.** The operator adds a native sidecar container to the preview replicas. Older versions of Kubernetes do not run it correctly.
* **Upgrade the Primary and workload operators together.** Mismatched versions refuse to establish the branch tunnel; non-branching previews are unaffected.
* If your workload namespaces restrict egress, **allow traffic to the operator's namespace on port `4980`** (the `db-tunnel` port on the operator Service).

`operator.multiCluster.preview.maxTunnelStreams` (default `256`) limits how many database connections all the preview replicas in one workload cluster can hold open to branch databases at the same time. It does not limit previews or branches - only open connections. A connection is one entry in an app's database pool: 20 running previews whose apps each pool 10 connections hold 200, and an idle preview holds none because its pods are gone. The default is sized well above what typical setups hold open at once; raise it if you run more. At the limit, new connections fail and the database driver retries; running connections are unaffected.

## RBAC — How Permissions Work

When the Primary operator connects to a downstream cluster, it needs permissions to list targets, create sessions, run health checks, and more. These permissions are set up automatically by the Helm chart on each downstream cluster.

The chart creates two ClusterRoles (permission definitions):

| ClusterRole | What it allows | Where it's granted |
|-------------|---------------|-------------------|
| `mirrord-operator-envoy` | General operations: listing targets, managing parent sessions, syncing database branches, reading pods/deployments, health checks | All member clusters |
| `mirrord-operator-envoy-remote` | Creating and managing child sessions | Member clusters only (not Primary) |

These multi-cluster roles are separate from the Operator API roles used by mirrord clients. Interactive users are usually bound to `mirrord-operator-user`, while CI runner identities for machine sessions such as `mirrord ci` and `mirrord preview` should be bound to the CI-specific `mirrord-operator-ci` role.

A ClusterRole by itself doesn't grant anything — it only defines what actions are possible. ClusterRoleBindings connect the ClusterRole to an identity (a ServiceAccount or a group).

### How Bindings Differ by Auth Type

| Auth type | Identity bound to ClusterRoles | How it's set up |
|-----------|-------------------------------|----------------|
| Bearer token / mTLS | `mirrord-operator-envoy` ServiceAccount | `multiClusterMember=true` creates the bindings |
| EKS IAM | `mirrord-operator-envoy` Kubernetes group | `multiClusterMemberIamGroup=mirrord-operator-envoy` creates the bindings |
| AKS Workload Identity | `mirrord-operator-envoy` Kubernetes group | `multiClusterMemberAzureGroup=mirrord-operator-envoy` creates the bindings |
| GKE | The Google identity, as a Kubernetes `User` | `multiClusterMemberGcpPrincipal=<service account email or principal://>` creates the bindings |

For EKS IAM, the Access Entry maps the IAM role to the `mirrord-operator-envoy` Kubernetes group. The ClusterRoleBindings grant permissions to that group. So the chain is: **IAM role -> Access Entry -> Kubernetes group -> ClusterRoleBinding -> ClusterRole -> permissions**.

For AKS Workload Identity, Azure RBAC or a K8s ClusterRoleBinding maps the Managed Identity to the `mirrord-operator-envoy` group. The chain is: **Managed Identity -> Entra ID token -> Kubernetes group -> ClusterRoleBinding -> ClusterRole -> permissions**.

For GKE, the API server resolves the Google token to the identity itself, so the binding names it directly. The chain is: **Google identity (service account or federated principal) -> Google access token -> Kubernetes user -> ClusterRoleBinding -> ClusterRole -> permissions**.

In practice:

- **Bearer token / mTLS downstream cluster**: `multiClusterMember=true`
- **EKS IAM downstream cluster**: `multiClusterMember=true` + `multiClusterMemberIamGroup=mirrord-operator-envoy`
- **AKS Workload Identity downstream cluster**: `multiClusterMember=true` + `multiClusterMemberAzureGroup=mirrord-operator-envoy`
- **GKE downstream cluster**: `multiClusterMember=true` + `multiClusterMemberGcpPrincipal=<Google identity>`

---

## Verify the Connection

After installing the operator on all clusters, verify that the Primary can reach all downstream clusters:

```bash
kubectl --context <PRIMARY_CLUSTER> get mirrordoperators operator -o yaml
```

Look for the `status.connected_clusters` section:

```yaml
status:
  connected_clusters:
  - connected:
      license_fingerprint: S1hgumDqNoyDar...
      operator_version: 3.137.0
    lastCheck: "2026-01-27T09:22:55.824115967+00:00"
    name: cluster-a
  - connected:
      license_fingerprint: S1hgumDqNoyDar...
      operator_version: 3.137.0
    lastCheck: "2026-01-27T09:22:55.766848498+00:00"
    name: cluster-b
```

Each connected cluster should show `license_fingerprint` and `operator_version`. If a cluster is not connected, you'll see an `error` field instead.

---

## Token Refresh

| Auth type | Refresh mechanism | Notes |
|-----------|------------------|-------|
| Bearer Token | Automatic via TokenRequest API | Refreshed at the halfway point of remaining lifetime. Only the initial token is manual. |
| EKS IAM | Automatic every 10 minutes | Tokens are presigned STS URLs, valid for 15 minutes. No Secrets involved. |
| AKS Workload Identity | Automatic every ~30 minutes | Tokens from Entra ID, valid for ~1 hour. Refreshed at the halfway point. No Secrets involved. |
| GKE | Automatic at the halfway point of the remaining lifetime | Google access tokens, valid for ~1 hour. On a GKE Primary the metadata server serves a cached token until about 5 minutes before it expires, so a mint can return a token with part of its hour used, and the next mints follow at about 30, 15, 7.5 and 4 minutes. Google reports no lifetime for a federated principal's token (an AKS or EKS Primary), so the operator treats each one as short-lived, logs `Google reports no lifetime for this GKE token` and mints every 3 minutes. No Secrets involved. |
| mTLS | **Not auto-refreshed** | You must rotate certificates manually before they expire. |

---

## FAQ

**Q: Do developers need to know about multi-cluster?**
A: No. The developer experience is identical to single-cluster. Developers run `mirrord exec` as usual and the operator handles everything. Note that multi-cluster sessions only work when the developer connects to the Primary cluster — connecting directly to a downstream cluster will start a regular single-cluster session on that cluster.

**Q: Can the Primary cluster also run workloads?**
A: Yes. Set `managementOnly: false` (the Helm chart default is `true`) so the Primary participates as a workload cluster, and set `managementOnly: true` only if the Primary has no application pods. See [Management-Only Mode](multi-cluster.md#management-only-mode) for what the setting changes.

**Q: Can I run some sessions on the Primary only?**
A: Yes, if the Primary runs workloads (`managementOnly: false`). A session started with `multi_cluster: false` stays on the Primary and runs as a regular single-cluster session, with the Primary's local queue splitting and database branching. With `managementOnly: true`, `multi_cluster: false` is not supported.

**Q: Which resources can I exclude from syncing to the member clusters?**
A: Label a `MirrordSplitConfig` or `MirrordPropertyList` with `operator.metalbear.co/skip-mc-sync: "true"` to keep the operator from syncing it. This is useful when your GitOps tool already applies the resource on every cluster. The operator skips it silently.

**Q: What happens if a downstream cluster is unreachable?**
A: The session creation will fail. The operator reports connection errors in the `MirrordOperator` status (see Verify the Connection above).

**Q: Can I mix authentication methods?**
A: Yes. Each downstream cluster can use a different `authType`. You can have some clusters using bearer tokens, others using EKS IAM, and others using mTLS.

**Q: Do I need the same operator version on all clusters?**
A: It's recommended. Version mismatches may cause compatibility issues.

**Q: Does the IAM role need AWS permissions?**
A: No. The IAM role used for EKS IAM authentication has zero AWS permissions. It's only used as an identity. All actual permissions come from Kubernetes RBAC on the downstream clusters via the Access Entry and ClusterRoleBindings.

**Q: Does the Azure Managed Identity need Azure permissions?**
A: Only the "Azure Kubernetes Service Cluster User Role" on each downstream cluster (if using Azure RBAC). The Managed Identity itself has no other Azure permissions - it's only used as a Kubernetes identity. All actual session permissions come from Kubernetes RBAC.

**Q: What if Entra ID is temporarily unavailable during AKS token refresh?**
A: The operator retries with exponential backoff (up to 15 minutes). Unlike bearer token auth, AKS Workload Identity has no chicken-and-egg problem - even if the old token expired days ago, the operator can always generate a fresh one because it uses the pod's projected SA token (managed by kubelet, always valid) to authenticate with Entra ID.
