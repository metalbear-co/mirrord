---
title: Security
date: 2022-07-10T08:48:57.000Z
lastmod: 2026-10-02T00:00:00.000Z
draft: false
images: []
linktitle: Security
menu:
  docs:
    teams: null
weight: 510
toc: true
tags:
  - team
  - enterprise
description: "How mirrord for Teams handles your data, access control, and compliance, for security teams that evaluate it."
---

# Security

{% hint style="info" %}
This discussion is only relevant for users on the Team and Enterprise pricing plans.
{% endhint %}

Love using mirrord but need help getting your security team on board? Talk to one of our technical experts!

<a href="https://metalbear.com/contact/" class="button primary">Get Security Support</a>

You can also visit our [Trust Center](https://trust.metalbear.com) for an overview of MetalBear's security posture, certifications, and compliance documentation.

## I'm a Security Engineer evaluating mirrord for Teams, what do I need to know?

* mirrord for Teams is completely on-prem. The only data sent to our cloud is license verification and usage metrics (see [details below](#what-data-does-the-mirrord-operator-send-to-metalbear-cloud)), which can be customized or disabled upon request. Usage metrics include developer usernames and session targets when identity sharing is on, which is the default for cloud API keys; your organization can turn it off to keep them anonymized.
* mirrord does not require root permissions on the user's machine.
* mirrord for Teams uses Kubernetes RBAC, meaning it doesn't add a new attack vector to your cluster.
* Communication between the mirrord client and the mirrord Operator takes place over your existing Kubernetes API. If you’ve configured your cluster to encrypt this communication (as is commonly done), then mirrord for Teams’ client-server communication is encrypted as well.
* mirrord for Teams defines a new CRD that can be used to limit access and use of mirrord, with plans of more fine-grained permissions in the future.
* The operator requires permissions to create a pod with the following capabilities in its Kubernetes namespace:
  * `CAP_NET_ADMIN` - for modifying routing tables
  * `CAP_SYS_PTRACE` - for reading the target pod's environment variables
  * `CAP_SYS_ADMIN` - for joining the target pod's network namespace
* The operator requires exclusions from the following gatekeeper policies:
  * `runAsNonRoot` - to access target pod's filesystem
  * `HostPath volume`/`Sharing the host namespace` - to access target pod's file system and networking
* With the experimental [MetalBear Cloud sessions-manager](../using-mirrord/serverless/metalbear-cloud.md), serverless workloads authenticate with a MetalBear API key exchanged for short-lived tokens, developers with their MetalBear account through `mirrord login`, and every session is scoped to your organization. See [How do serverless workloads authenticate to MetalBear Cloud?](#how-do-serverless-workloads-authenticate-to-metalbear-cloud).
* With the experimental [Operator-hosted sessions-manager](../using-mirrord/serverless/operator-hosted.md), workloads outside Kubernetes, such as Amazon ECS tasks, reach the Operator through your EKS API server as Kubernetes identities mapped from their IAM roles, with no shared secrets. See [How do ECS workloads authenticate to the Operator?](#how-do-ecs-workloads-authenticate-to-the-operator).
* Operator activity is logged per session, including the Kubernetes user, the target, and the traffic filter in use. See [Auditing mirrord usage](#how-do-i-audit-mirrord-usage).
* mirrord can run fully air-gapped, with no outbound communication to MetalBear. See [Air-gapped operation](#can-mirrord-run-air-gapped).
* Released container images and CLI binaries carry signed SLSA Build Level 2 provenance, so you can verify what you pulled before installing it. See [How is the Operator built and distributed](#how-is-the-operator-built-and-distributed).
* Missing anything? Feel free to ask us on [Slack](https://metalbear.com/slack) or hi@metalbear.com

## How do I audit mirrord usage?

The Operator logs an event for every session at `INFO` level, which is the default log level. To get these in a form your logging or SIEM stack can ingest, set `operator.jsonLog` to `true` in the Operator Helm chart values.

See [Monitoring](monitoring.md) for the full field reference and for Prometheus, OpenTelemetry, DataDog, Grafana, and fluentd/Elasticsearch integration.

Logged events include `Session Start`, `Session End`, `Port Steal`, `Port Mirror`, `Port Release`, and `Copy Target`. Fields relevant to an audit trail include:

* `client_user` - the Kubernetes user of the client, resolved via Kubernetes RBAC
* `client_hostname`, `client_name`, `client_id` - identity of the machine and client certificate
* `target` - the session's target
* `session_id`, `session_duration` - correlation and length of each session
* `http_filter` - the client's configured HTTP filter

Together with the session start and end times, these fields let you attribute mirrord sessions to individual Kubernetes users and targets, including when several engineers are working against the same service concurrently.

## Can mirrord run air-gapped?

Yes, on the Enterprise plan. In an air-gapped deployment the Operator sends no telemetry or license verification traffic to MetalBear. Running the [License Server](license-server.md) on-prem is optional, but it is the recommended way to manage seats and collect usage without MetalBear's cloud.

## How is the Operator built and distributed?

* The mirrord agent is [open source](https://github.com/metalbear-co/mirrord) and can be audited directly.
* The Operator is distributed as a versioned container image from `ghcr.io/metalbear-co/operator`, installed via our [public Helm charts](https://github.com/metalbear-co/charts). Each chart release pins a matching `appVersion`, so the chart and the images it deploys move together.
* Because you control when you bump the chart version, you control when a new Operator or agent image enters your cluster. Chart releases are public and can be reviewed before you upgrade.
* Released container images and CLI binaries are published with signed [SLSA](https://slsa.dev) Build Level 2 provenance. It is generated by our release pipeline on GitHub Actions, signed through Sigstore, and published alongside the artifact, so you can confirm that what you pulled was built by us from the source you expect.

### Verifying build provenance

Attestations are published to GHCR alongside each image, and to the GitHub release for CLI binaries, under the `metalbear-co` owner. Verify them with [`gh attestation verify`](https://cli.github.com/manual/gh_attestation_verify), for example:

```bash
gh attestation verify \
  oci://ghcr.io/metalbear-co/operator:<version> --owner metalbear-co
```

The same applies to `ghcr.io/metalbear-co/mirrord` and to CLI binaries downloaded from a release. Verification needs a GitHub token with the `read:packages` scope, but no access to MetalBear's organization or repositories.

For our vulnerability disclosure and customer notification process, see the [Trust Center](https://trust.metalbear.com).

## What data does the mirrord Operator send to MetalBear cloud?

mirrord for Teams is completely on-prem. The Operator communicates with MetalBear servers over an encrypted TLS connection only for license verification and usage metrics. What the usage metrics contain depends on your organization's identity sharing setting.

### Always sent (anonymized), except on Enterprise air-gapped

These fields are always sent on every plan. The only exception is an Enterprise [air-gapped](#can-mirrord-run-air-gapped) deployment, where nothing is sent to MetalBear. Running the on-prem [License Server](license-server.md) there is optional, but it is the recommended way to collect usage without MetalBear's cloud.

1. User ID (randomly generated hash, stored on user machine)
2. Duration of session
3. Organization name
4. mirrord License Hash (of organization)
5. instance_id (generated on runtime per Operator pod)
6. subscription_id (generated uuid)
7. organization_id (generated uuid)
8. cluster_id (the UID of the cluster's `default` namespace, used as a stable, anonymous per-cluster identifier)
9. kubernetes_version (the version of the Kubernetes cluster the Operator is running in)

Operators older than 3.227.0 also send cluster_name (optional; set via the `operator.clusterName` Helm value) in this list. From 3.227.0 it is sent only with identity sharing enabled, see below.

None of these fields identify an individual developer or a workload in your cluster.

### Sent only with identity sharing enabled

Identity sharing is an organization-level setting. An organization admin chooses it when generating a [cloud API key](operator.md#cloud-api-key) at [app.metalbear.com](https://app.metalbear.com), where it is ticked by default, and can change it later for the current key on the same page. With it enabled, the Operator's events also carry, where applicable:

1. Kubernetes username of the client, as authenticated by your Kubernetes API server
2. Display name of the local account and hostname of the client machine
3. Namespace, kind, name, and container of the session's target
4. cluster_name, from Operator 3.227.0 (optional; only present if you set the `operator.clusterName` Helm value to give the cluster a recognizable label). It is shown in the Clusters view at app.metalbear.com. Without identity sharing, clusters are listed by `cluster_id` only. A self-hosted [License Server](license-server.md) receives it regardless, since that data never leaves your infrastructure.

These are what the usage dashboard at app.metalbear.com uses to show usernames and service names instead of hashes. Anonymized metrics go to `analytics.metalbear.com`; events that carry identity go to `app.metalbear.com`.

Identity is not sent when:

* The cloud API key was generated with identity sharing unticked, or the organization's key predates the setting and it was never turned on.
* `cloud.anonymizeData` is `true` in the Operator's Helm values. This overrides the key's setting.
* The Operator authenticates with a legacy license key instead of a cloud API key.
* The Operator authenticates against a self-hosted [License Server](license-server.md). A cloud API key is ignored in that configuration and usage metrics stay anonymized.

Changing the setting takes effect at the Operator's next cloud token refresh, without regenerating the key or restarting the Operator.

Only the Enterprise plan can disable this communication entirely, through an [air-gapped](#can-mirrord-run-air-gapped) deployment (the on-prem License Server is the recommended way to collect usage there). On other plans the anonymized fields above are always sent.

## Are you SOC2/ISO27001 compliant?

Yes, MetalBear is SOC2 Type II and ISO27001 certified.

## How do I configure Role Based Access Control for mirrord for Teams?

mirrord for Teams works on top of Kubernetes' built-in RBAC with the following resources, `mirrordoperators`, `mirrordoperators/certificate`, `targets`, and `targets/port-locks` under the `operator.metalbear.co` apiGroup. The first two resources are required at a cluster level, and the last two can be allowed at a namespace level.

With the experimental `operator.sessionsManager` enabled, which lets developers target [serverless workloads](../using-mirrord/serverless/README.md) such as Amazon ECS tasks, the Operator also serves three cluster-scoped resources in the same apiGroup: `serverlessclientassignments` and `serverlessagentassignments`, with the `proxy` verb, and `serverlessdataplanes`, with the `get` verb. The developer's local mirrord registers on `serverlessclientassignments` (the **client** side) and the workload on `serverlessagentassignments` (the **agent** side), so RBAC decides which side of a session an identity may act as; both sides then connect to their session through `serverlessdataplanes`.

You can limit a user's ability to use mirrord on specific targets by limiting their access to the `target` resource. The specific verbs for rules to our resources can be copied from the examples below.

For your convenience, mirrord for Teams includes built-in ClusterRoles that control access to the Operator API:

- `mirrord-operator-user` for interactive users running mirrord locally.
- `mirrord-operator-ci` for machine sessions in CI runners, such as `mirrord ci` and `mirrord preview`.

Both roles grant access to the same Operator API resources by default, but keeping CI access in a separate role lets you bind and label machine identities independently from human users. To grant access to the Operator API, you can create a ClusterRoleBinding like this:

```yaml

apiVersion: rbac.authorization.k8s.io/v1
kind: ClusterRoleBinding
metadata:
  name: mirrord-operator-rolebinding
subjects:
- kind: User
  name: jim
  apiGroup: rbac.authorization.k8s.io
roleRef:
  kind: ClusterRole
  name: mirrord-operator-user
  apiGroup: rbac.authorization.k8s.io
```

For CI runners, bind the runner's ServiceAccount, Kubernetes group, or other authenticated identity to `mirrord-operator-ci` instead of `mirrord-operator-user`.

With `operator.sessionsManager` enabled, the built-in `mirrord-operator-user`, `mirrord-operator-ci` and `mirrord-operator-user-basic` roles also grant `proxy` on `serverlessclientassignments` and `get` on `serverlessdataplanes`, so developers bound to them can target serverless workloads with no further RBAC changes, but can't register as a workload. The chart then also creates the `mirrord-operator-sessions-manager-agent` ClusterRole for workloads, such as ECS tasks, that register with the Operator: it grants only `proxy` on `serverlessagentassignments` and `get` on `serverlessdataplanes`, and none of the user permissions. Bind it to the Kubernetes identity the workload authenticates as; see [Operator-Hosted Sessions-Manager](../using-mirrord/serverless/operator-hosted.md#connecting-workloads). Registrations are named `<environment>.<service>`, so you can limit a workload identity to specific services with `resourceNames`; see [Limiting a workload to specific services](../using-mirrord/serverless/operator-hosted.md#limiting-a-workload-to-specific-services).

In addition, the Operator verifies each caller's permissions with a Kubernetes `SubjectAccessReview` for the calling user's identity (user, groups and extra fields), and thus only operates on pods or deployments for which the user has `get` permissions. The Operator does not impersonate the user when making this check or when acting on their behalf.

To see the latest definition, we recommend checking our [Helm chart](https://github.com/metalbear-co/charts/blob/main/mirrord-operator/templates/cluster-role.yaml).

### How do I limit user access to a specific namespace?

Create a ClusterRoleBinding between the user and the `mirrord-operator-user-basic` role, then create a [namespaced role](https://github.com/metalbear-co/charts/blob/main/mirrord-operator/templates/namespaced-role.yaml) (easiest via Helm chart by specifying `roleNamespaces`) and bind create RoleBinding in the namespace.

### How do I limit user access to a specific target?

If the user doesn't have `get` access to the targets, then they won't be able to target them with mirrord. However, if you want to allow `get` access to targets but disallow using mirrord on them, we recommend creating a new role based on the `mirrord-operator-user` namespaced role above, and adding a `resourceNames` field to the `targets` resource. This will limit the user to only using the Operator on the specified targets. For example:

```yaml
- apiGroups:
  - operator.metalbear.co
  resources:
  - targets
  resourceNames:
  - "deployment.my-deployment"
  - "pod.my-pod"
  - "rollout.my-argo-rollout"
  verbs:
  - proxy
```

## How do ECS workloads authenticate to the Operator?

When developers target an Amazon ECS task as a [serverless workload](../using-mirrord/serverless/README.md) in the Operator-hosted deployment mode, a mirrord component inside the ECS task connects to the Operator through your EKS API server, the same way developers do. It authenticates as a Kubernetes identity and is authorized by ordinary Kubernetes RBAC:

* The ECS task's IAM task role is mapped to a Kubernetes username and group with an [EKS access entry](https://docs.aws.amazon.com/eks/latest/userguide/access-entries.html). The entry has no access policy.
* The task signs the same token `aws eks get-token` produces, locally from its task role's credentials. Tokens are valid for 15 minutes and refreshed before they expire. The task role needs no IAM permissions.
* A ClusterRoleBinding grants that group the chart's `mirrord-operator-sessions-manager-agent` ClusterRole, which allows only registering for sessions and connecting to them. The task can't read, list or modify any other resource in the cluster.
* There are no shared secrets to create, distribute or rotate, and no AWS credentials are added to the task.
* Each data-plane connection is bound to its session by a single-use credential delivered with the session assignment, so an identity allowed to connect to sessions can't attach to another caller's session.
* The Operator isn't exposed outside the cluster: both sides only make outbound HTTPS connections to the EKS API server. Requests from the task appear in the cluster's audit log under the access entry's username.

To revoke access, delete the access entry or the ClusterRoleBinding. For the full setup and a step-by-step description of the token flow, see [Connecting ECS to the Operator](../using-mirrord/serverless/ecs-operator-hosted.md#how-ecs-tasks-authenticate). For how the Operator-hosted mode works overall, see [Operator-Hosted Sessions-Manager](../using-mirrord/serverless/operator-hosted.md#how-it-works).

## How do serverless workloads authenticate to MetalBear Cloud?

When developers target a [serverless workload](../using-mirrord/serverless/README.md) without the Operator, in the experimental MetalBear Cloud deployment mode, both the workload and the developer's local mirrord connect to a sessions-manager hosted by MetalBear:

* The workload holds a MetalBear API key issued for serverless workloads. Other MetalBear credentials, such as the Operator's license key, are refused.
* Before registering, the workload exchanges the key at `app.metalbear.com` for a short-lived token over HTTPS, and presents only the token to sessions-manager. Neither the key nor the tokens are logged.
* Developers authenticate with the token `mirrord login` stores when they sign in to their MetalBear account, which names them and their organization. They never handle the API key.
* Sessions-manager verifies both tokens against MetalBear's signing keys, and accepts each only for its own side: the workload's token can't start a developer session, and a developer's token can't register a workload.
* Every registration is scoped to the organization its token names, so workloads and developers are only paired within your organization.
* Each data-plane connection is bound to its session by a single-use credential delivered with the session assignment, so a caller can't attach to another caller's session.
* Both sides only make outbound HTTPS connections; nothing in your infrastructure accepts inbound connections.
* The key authenticates your organization, not a specific workload: anyone holding it can register a workload under any of your services and environments. Store it in a secret store, such as AWS Secrets Manager, and rotate it if it leaks.

For the full setup, see [MetalBear Cloud Sessions-Manager](../using-mirrord/serverless/metalbear-cloud.md).

## How can I prevent users in my team from stealing or mirroring traffic from a target?

You can define [policies](../sharing-the-cluster/policies.md) that prevent stealing (or only prevent stealing without setting a filter) and/or mirroring for selected targets. Let us know if there are more features you would like to be able to limit using policies.

## How can I prevent users from using mirrord without going through the Operator?

When the mirrord CLI starts, it checks if an Operator is installed in the cluster and uses it if it's available. However, if the user lacks access to the Operator or if the Operator doesn't exist, mirrord attempts to create an agent directly.

To prevent clients from attempting to create an agent without the Operator, you can add the [following key](https://metalbear.com/mirrord/docs/config#operator) to the mirrord configuration file:

```json
{
  "operator": true
}
```

To prevent mirrord clients from directly creating agents at the cluster level, we recommend disallowing the creation of pods with extra capabilities by using [Pod Admission Policies](https://kubernetes.io/tasks/configure-pod-container/enforce-standards-namespace-labels/). Apply a baseline or stricter policy to all namespaces while excluding the mirrord namespace.

Note: before adding a new Pod Admission Policy, you should make sure it doesn't limit any functionality required by your existing workloads.

By default the in-cluster traffic between the operator and its agents isn't encrypted nor authenticated. To ensure encryption and authentication you can enable TLS protocol for the operator–agent connections. You can do this in the operator [Helm chart](https://github.com/metalbear-co/charts/blob/main/mirrord-operator/values.yaml) by setting `agent.tls` to true or manually by setting `OPERATOR_AGENT_CONNECTION_TLS=true` in the operator container environment. TLS connections are supported from agent version 3.97.0.

## Security hardening with the mirrord operator

Here is a quick checklist you may wish to follow in order to improve the security posture of your cluster when using the operator:

### Enabling TLS

TLS can be enabled between the operator and mirrord agents to encrypt the traffic they send to each other. From the [section above](security.md#how-can-i-prevent-users-from-using-mirrord-without-going-through-the-operator):

> By default the in-cluster traffic between the operator and its agents isn’t encrypted nor authenticated. To ensure encryption and authentication you can enable TLS protocol for the operator–agent connections. You can do this in the operator [Helm chart](https://github.com/metalbear-co/charts/blob/main/mirrord-operator/values.yaml) by setting `agent.tls` to true or manually by setting `OPERATOR_AGENT_CONNECTION_TLS=true` in the operator container environment. TLS connections are supported from agent version 3.97.0.

### Reducing access to the mirrord namespace

Users have no need to access to the namespace where mirrord resources are created. By default, this is the `mirrord` namespace.

### Using a certificate for mirrord APIService

By using either your own certificate or one provided by a certificate manager, you can secure access to mirrord's APIService - you will need to set `insecureSkipTLSVerify` to `false` in the mirrord-operator Helm chart.

_NB: If you are using a certificate manager, make sure you set up reminders for certificate renewal._

### Set up network policies for communication

Access to the operator can be further restricted by setting up [network policies](https://kubernetes.io/concepts/services-networking/network-policies/) in the cluster to limit the operator to communicate only with mirrord agents (this is not possible if running agents in [ephemeral mode](https://metalbear.com/mirrord/docs/config#agent.ephemeral)).
