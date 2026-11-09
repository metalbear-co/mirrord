---
title: "Connecting ECS to the Operator"
description: "Connect an Amazon ECS task to the Operator-hosted sessions-manager: map the task role into the EKS cluster, make the API server reachable from ECS, and add the connection variables to the task."
tags:
  - experimental
  - enterprise
---

# Connecting ECS to the Operator

{% hint style="warning" %}
Connecting ECS to the Operator is **experimental**, and may change without notice. Set up the [Operator-hosted sessions-manager](operator-hosted.md) and [remote bootstrap](ecs.md) before following this guide.
{% endhint %}

This guide connects an ECS task that runs the [mirrord remote bootstrap](ecs.md) to the [Operator-hosted sessions-manager](operator-hosted.md) on your EKS cluster. The task authenticates as its IAM task role, mapped to a Kubernetes identity. At a glance, the setup is:

1. Bind the `mirrord-ecs-workloads` group to the chart's `mirrord-operator-sessions-manager-agent` ClusterRole ([Grant the ECS task access](#grant-the-ecs-task-access)).
2. Map the ECS task role into that group with an EKS access entry ([Map the ECS task role into the cluster](#map-the-ecs-task-role-into-the-cluster)).
3. Make the EKS API server's private endpoint reachable and resolvable from the ECS VPC over a Transit Gateway ([Make the EKS API server reachable from ECS](#make-the-eks-api-server-reachable-from-ecs)).
4. Add the connection variables to the task and deploy it ([Add the connection variables](#add-the-connection-variables)).
5. [Verify](#verify) that the task registers and a developer session works.

## How ECS tasks authenticate

{% hint style="info" %}
This section is for whoever reviews the setup, such as your security team. To start the setup, skip to [Prerequisites](#prerequisites). For how the Operator-hosted mode works overall, see [Operator-Hosted Sessions-Manager](operator-hosted.md#how-it-works).
{% endhint %}

The ECS task does the same thing `aws eks get-token` does, without the AWS CLI:

1. The mirrord remote bootstrap reads the task role's credentials from the ECS container credentials endpoint, through the standard AWS SDK credential chain. They're refreshed automatically.
2. It signs, locally and without any network call, a presigned STS `GetCallerIdentity` request that includes the header `x-k8s-aws-id: <cluster name>`, and sends it to the EKS API server as a bearer token.
3. EKS validates the token with STS, resolves the IAM role, and looks up its [access entry](https://docs.aws.amazon.com/eks/latest/userguide/access-entries.html), which gives it a Kubernetes username and groups.
4. Kubernetes RBAC decides whether that identity may use the sessions-manager resources, and the request is forwarded to the Operator.

EKS tokens are valid for 15 minutes, or until the task role credentials they were signed with expire, if that's sooner. The bootstrap signs a fresh one 5 minutes before the current one expires, which is local and cheap. If the task role's credentials are briefly unavailable, it retries with backoff until the current token expires, and then stops with an error. The API server authenticates a request only when it starts, so the long-lived connections of an ongoing session aren't cut when a token expires; only new requests use the new token.

The task role needs **no IAM permissions** for this: `sts:GetCallerIdentity` is allowed for every AWS identity, and the ECS task never calls STS or EKS APIs itself. No AWS credentials, access keys or secrets are added to the task.

This authentication flow is the same on **Fargate and EC2**. On EC2, use the task IAM role, not the container instance's IAM role. ECS-optimized AMIs provide task-role credential delivery; with a custom AMI, ensure the ECS agent and container networking allow the application container to reach the ECS credentials endpoint. The `bridge` and `host` network modes may need additional agent configuration and host networking rules; follow AWS's [EC2 task-role configuration requirements](https://docs.aws.amazon.com/AmazonECS/latest/developerguide/task-iam-roles.html#ecs-task-iam-roles-additional-configuration-for-ec2).

## Prerequisites

1. The [Operator-hosted sessions-manager](operator-hosted.md) enabled on the EKS cluster.
2. The ECS task definition set up with the [remote bootstrap](ecs.md), up to [Connect to your deployment mode](ecs.md#connect-to-your-deployment-mode).
3. The cluster's authentication mode set to `API` or `API_AND_CONFIG_MAP`, so access entries can be used. [Map the ECS task role into the cluster](#map-the-ecs-task-role-into-the-cluster) shows how to check and change it.
4. The ECS VPC and the EKS VPC attached to the same Transit Gateway.
5. An IAM **task role** on the ECS service (`taskRoleArn` in the task definition), with credentials available inside the application container. This is not the execution role or, for EC2-backed tasks, the container instance role.
6. `kubectl` and the AWS CLI, with permission to manage cluster RBAC, EKS access entries, VPC networking and the ECS task definition.

Throughout this guide, replace:

| Placeholder | Value |
| --- | --- |
| `<CLUSTER_NAME>` | The EKS cluster's name |
| `<REGION>` | The EKS cluster's AWS region |
| `<TASK_ROLE_ARN>` | The ARN of the ECS service's task role |
| `<ECS_VPC_CIDR>` | The CIDR range of the ECS tasks' VPC |
| `<YOUR_SERVICE_NAME>` | The service name set in `MIRRORD_REMOTE_SERVICE` on the task |

---

## Grant the ECS task access

Bind the chart's `mirrord-operator-sessions-manager-agent` ClusterRole to a group that the ECS task role will be mapped into in the next step. Save this as `mirrord-ecs-workload-rbac.yaml`:

```yaml
apiVersion: rbac.authorization.k8s.io/v1
kind: ClusterRoleBinding
metadata:
  name: mirrord-ecs-workload
roleRef:
  apiGroup: rbac.authorization.k8s.io
  kind: ClusterRole
  name: mirrord-operator-sessions-manager-agent
subjects:
  - apiGroup: rbac.authorization.k8s.io
    kind: Group
    name: mirrord-ecs-workloads
```

and apply it:

```bash
kubectl apply -f mirrord-ecs-workload-rbac.yaml
```

The role grants `proxy` on `serverlessagentassignments` (the **agent** side: registering and waiting for sessions, a long-lived request) and `get` on `serverlessdataplanes` (connecting to an assigned session, a WebSocket), and nothing else in the cluster: the ECS task can't read, list or modify any other resource. These resources are cluster-scoped, so the binding has to be a ClusterRoleBinding. You only need one binding for all your ECS services. To limit task roles to specific services instead, see [Limiting a workload to specific services](operator-hosted.md#limiting-a-workload-to-specific-services).

## Map the ECS task role into the cluster

An [EKS access entry](https://docs.aws.amazon.com/eks/latest/userguide/access-entries.html) tells EKS which Kubernetes identity an IAM role authenticates as.

If the cluster still uses only the `aws-auth` ConfigMap, first allow access entries alongside it. This doesn't change any existing `aws-auth` mapping:

```bash
aws eks describe-cluster --name <CLUSTER_NAME> --region <REGION> \
  --query 'cluster.accessConfig.authenticationMode' --output text

# Only if the above printed CONFIG_MAP:
aws eks update-cluster-config --name <CLUSTER_NAME> --region <REGION> \
  --access-config authenticationMode=API_AND_CONFIG_MAP \
  --query update.id --output text
```

The change takes a few minutes. Before creating the access entry, wait until the update's status is `Successful`, using the update ID the command printed:

```bash
aws eks describe-update --name <CLUSTER_NAME> --region <REGION> \
  --update-id <UPDATE_ID> --query update.status --output text
```

Then create the access entry for the task role, placing it in the group bound in the previous step:

```bash
aws eks create-access-entry \
  --cluster-name <CLUSTER_NAME> \
  --region <REGION> \
  --principal-arn <TASK_ROLE_ARN> \
  --type STANDARD \
  --username mirrord-ecs-<YOUR_SERVICE_NAME> \
  --kubernetes-groups mirrord-ecs-workloads
```

* `--principal-arn` is the **role** ARN (`arn:aws:iam::<account>:role/<name>`), not an `assumed-role` session ARN, and it must be the task role, not the execution role.
* Don't associate any access policy with this entry. Its permissions come only from the ClusterRoleBinding in the previous step.
* `--username` is how the task appears in the Kubernetes audit log and in `kubectl auth can-i`. Any name works; including the service name makes audit entries easy to attribute.

Check the result by impersonating that identity:

```bash
kubectl auth can-i proxy serverlessagentassignments.operator.metalbear.co \
  --as mirrord-ecs-<YOUR_SERVICE_NAME> --as-group mirrord-ecs-workloads      # yes (agent: workloads)
kubectl auth can-i get serverlessdataplanes.operator.metalbear.co \
  --as mirrord-ecs-<YOUR_SERVICE_NAME> --as-group mirrord-ecs-workloads      # yes
kubectl auth can-i proxy serverlessclientassignments.operator.metalbear.co \
  --as mirrord-ecs-<YOUR_SERVICE_NAME> --as-group mirrord-ecs-workloads      # no  (client: developers)
kubectl auth can-i list pods -A \
  --as mirrord-ecs-<YOUR_SERVICE_NAME> --as-group mirrord-ecs-workloads      # no
```

## Make the EKS API server reachable from ECS

The ECS task connects to the cluster's **private** API server endpoint over the Transit Gateway. The private endpoint is a set of network interfaces EKS places in the cluster VPC's subnets; they carry the cluster security group. You only need to do this once per ECS VPC.

The source of the connection depends on the task's network mode, not just its launch type:

* **`awsvpc`** (required on Fargate, also available on EC2): use the task ENI's subnet routes and security group.
* **`bridge` or `host`** (EC2): use the container instance's subnet routes and security group. In `bridge` mode, outbound traffic is normally translated to the instance's IP address; the task doesn't have its own VPC security group.

In either case, the application container must resolve the EKS endpoint and reach it on TCP 443. Apply the routing and firewall checks below to the task ENI or container instance as appropriate.

### Enable the private endpoint

```bash
aws eks describe-cluster --name <CLUSTER_NAME> --region <REGION> \
  --query 'cluster.resourcesVpcConfig.{private:endpointPrivateAccess,public:endpointPublicAccess}'

# Only if `private` is false:
aws eks update-cluster-config --name <CLUSTER_NAME> --region <REGION> \
  --resources-vpc-config endpointPrivateAccess=true
```

Enabling private access doesn't change public access, so developers who use the public endpoint are unaffected.

### Routing

* The Transit Gateway route table associated with the ECS VPC attachment routes the EKS VPC CIDR to the EKS VPC attachment, and the one associated with the EKS VPC attachment routes `<ECS_VPC_CIDR>` back (static routes or propagation).
* The route tables of the ECS source subnets send the EKS VPC CIDR to the Transit Gateway: task ENI subnets for `awsvpc`, or container instance subnets for `bridge`/`host`.
* The route tables of the EKS subnets holding the endpoint interfaces send `<ECS_VPC_CIDR>` to the Transit Gateway.

### Firewalling

* The cluster security group allows inbound TCP 443 from `<ECS_VPC_CIDR>`:

  ```bash
  CLUSTER_SG=$(aws eks describe-cluster --name <CLUSTER_NAME> --region <REGION> \
    --query 'cluster.resourcesVpcConfig.clusterSecurityGroupId' --output text)
  aws ec2 authorize-security-group-ingress --region <REGION> --group-id "$CLUSTER_SG" \
    --protocol tcp --port 443 --cidr <ECS_VPC_CIDR>
  ```

  Use a CIDR rule: a security group in the ECS VPC can only be referenced across a Transit Gateway if security group referencing is turned on for it.
* The source security group allows outbound TCP 443 to the EKS VPC CIDR: the task ENI's security group for `awsvpc`, or the container instance's security group for `bridge`/`host`.
* Network ACLs on both sides, and any firewall or inspection appliance on the Transit Gateway path, allow this traffic and its return traffic.

### DNS

The cluster endpoint's hostname (e.g. `https://ABC123.gr7.<REGION>.eks.amazonaws.com`) resolves to the private endpoint only from inside the EKS VPC. Queried from the ECS VPC, it resolves to the public endpoint instead (or doesn't resolve at all when public access is off), and traffic wouldn't take the Transit Gateway. Forward the name to the EKS VPC's resolver:

1. In the EKS VPC, create a Route 53 Resolver **inbound endpoint**, with a security group allowing DNS (TCP and UDP 53) from `<ECS_VPC_CIDR>`.
2. In the ECS VPC, create a Route 53 Resolver **outbound endpoint** and a **forwarding rule** for the endpoint's hostname (e.g. `ABC123.gr7.<REGION>.eks.amazonaws.com`) that targets the inbound endpoint's IP addresses, and associate the rule with the ECS VPC.

If your organization already centralizes DNS through Resolver endpoints on the Transit Gateway, add the forwarding rule there instead. Either way, from inside the ECS VPC the hostname must resolve to IP addresses within the EKS VPC CIDR.

### Long-lived connections

The Transit Gateway closes TCP connections idle for 350 seconds. The Operator's keepalives keep sessions well within that; see [Operating notes](operator-hosted.md#operating-notes).

{% hint style="warning" %}
Any firewall or proxy on the path between the ECS task and the EKS API server needs an idle timeout of at least 300 seconds and must not buffer responses.
{% endhint %}

## Add the connection variables

Collect the cluster's endpoint and certificate authority:

```bash
aws eks describe-cluster --name <CLUSTER_NAME> --region <REGION> \
  --query 'cluster.{endpoint:endpoint,ca:certificateAuthority.data}'
```

Set the task role on the task definition, and add the connection variables to the application container's `environment`, next to the variables from [Amazon ECS](ecs.md#name-the-workload):

```json
{
  "taskRoleArn": "<TASK_ROLE_ARN>",
  "containerDefinitions": [
    {
      "name": "app",
      "environment": [
        { "name": "MIRRORD_OPERATOR_API_URL", "value": "<CLUSTER_ENDPOINT>" },
        { "name": "MIRRORD_OPERATOR_EKS_CLUSTER_NAME", "value": "<CLUSTER_NAME>" },
        { "name": "MIRRORD_OPERATOR_API_CA_DATA", "value": "<CLUSTER_CA_DATA>" }
      ]
    }
  ]
}
```

| Variable | Value |
| --- | --- |
| `MIRRORD_OPERATOR_API_URL` | The `endpoint` printed above, e.g. `https://ABC123.gr7.<REGION>.eks.amazonaws.com`. Setting it selects the Operator-hosted mode; without it, the bootstrap registers with the MetalBear Cloud sessions-manager instead. |
| `MIRRORD_OPERATOR_EKS_CLUSTER_NAME` | `<CLUSTER_NAME>`. Setting it switches on authentication as the task role. It's signed into the token, and EKS rejects tokens for another cluster. |
| `MIRRORD_OPERATOR_API_CA_DATA` | The `ca` printed above (base64-encoded PEM). The API server's certificate is issued by the cluster's own CA, not a public one. |

These three are the same for every ECS service connecting to the cluster, so they're good candidates for a [shared environment file](ecs.md#sharing-settings-across-services).

{% hint style="warning" %}
Don't also set `MIRRORD_SESSIONS_MANAGER_URL` or `MIRRORD_OPERATOR_KUBE_CONTEXT` on the task. They select a different way of connecting, and the bootstrap refuses to start when more than one is set. Without `MIRRORD_OPERATOR_EKS_CLUSTER_NAME`, the bootstrap sends its requests without credentials, and the API server refuses them with `401 Unauthorized`, or with `403 Forbidden` for user `system:anonymous`.
{% endhint %}

The token is signed for the cluster's region, read from the `MIRRORD_OPERATOR_API_URL` hostname, so an EKS endpoint needs no extra configuration. Only if the task reaches the API server under another name, such as through a proxy, does the bootstrap fall back to `AWS_REGION`, then `AWS_DEFAULT_REGION`; set one of them to the cluster's region.

Register the new task definition revision and update the service to use it.

---

## Verify

1. **ECS task registered**: after the new task starts, the Operator logs its registration:

   ```bash
   kubectl -n mirrord logs deployment/mirrord-operator -f | grep -i session
   ```

   With [EKS control plane audit logging](https://docs.aws.amazon.com/eks/latest/userguide/control-plane-logs.html) enabled, requests from the task appear in the audit log with user `mirrord-ecs-<YOUR_SERVICE_NAME>` on resource `serverlessagentassignments`.
2. **Developer session**: with the `mirrord.json` from [Using mirrord with a Serverless Workload](README.md#using-mirrord-with-a-serverless-workload), `mirrord exec` connects, and a request with the `baggage` header reaches the local process.

## Infrastructure as code

If you manage your infrastructure with Terraform, each step maps to standard resources:

| Step | Terraform resources |
| --- | --- |
| Grant the ECS task access | `kubernetes_cluster_role_binding_v1` |
| Map the ECS task role | `aws_eks_access_entry` (`type = "STANDARD"`, `kubernetes_groups = ["mirrord-ecs-workloads"]`) |
| Routing | `aws_ec2_transit_gateway_vpc_attachment`, `aws_route` |
| Firewalling | `aws_vpc_security_group_ingress_rule`, `aws_vpc_security_group_egress_rule` |
| DNS | `aws_route53_resolver_endpoint` (inbound and outbound), `aws_route53_resolver_rule`, `aws_route53_resolver_rule_association` |
| Connection variables | `aws_ecs_task_definition` |

In CloudFormation, the access entry is `AWS::EKS::AccessEntry`.

## Revoking access

Delete the access entry (`aws eks delete-access-entry`) to revoke one task role, or the `mirrord-ecs-workload` ClusterRoleBinding to revoke every ECS task. New requests are refused immediately; to also end open sessions, restart the Operator.

## Troubleshooting

The bootstrap's messages appear in the application container's logs. For problems that don't depend on the deployment mode, see [Amazon ECS](ecs.md#troubleshooting). With `MIRRORD_AGENT_RUST_LOG=info` in the application container's `environment`, the bootstrap also logs which sessions-manager it registers with: `Registering with the sessions-manager hosted by the mirrord operator, selected by MIRRORD_OPERATOR_API_URL`.

| Symptom | Likely cause |
| --- | --- |
| The ECS task logs a connection timeout to the API server | Transit Gateway routes, security groups or NACLs ([Make the EKS API server reachable from ECS](#make-the-eks-api-server-reachable-from-ecs)). |
| The endpoint hostname resolves to public IPs from the ECS VPC | The DNS forwarding rule is missing or not associated with the ECS VPC ([DNS](#dns)). |
| TLS or certificate verification error | `MIRRORD_OPERATOR_API_CA_DATA` is missing or belongs to another cluster. |
| `401 Unauthorized` | `MIRRORD_OPERATOR_EKS_CLUSTER_NAME` isn't set, no access entry for the role, the access entry uses the execution role instead of the task role, `MIRRORD_OPERATOR_EKS_CLUSTER_NAME` doesn't match the cluster, or the authentication mode is still `CONFIG_MAP`. |
| `403 Forbidden` on `serverlessagentassignments` | The user is `system:anonymous` because `MIRRORD_OPERATOR_EKS_CLUSTER_NAME` isn't set, or the access entry's group isn't `mirrord-ecs-workloads`, or the ClusterRoleBinding is missing. Check with `kubectl auth can-i` ([Map the ECS task role into the cluster](#map-the-ecs-task-role-into-the-cluster)). |
| `404 Not Found` | `operator.sessionsManager` isn't enabled, or the Operator image is older than **3.223.0**. Check any image or tag overrides in your Helm values ([Enable sessions-manager](operator-hosted.md#enable-sessions-manager)). |
| `MIRRORD_SESSIONS_MANAGER_URL, MIRRORD_OPERATOR_API_URL and MIRRORD_OPERATOR_KUBE_CONTEXT are mutually exclusive` | The task sets more than one; keep only `MIRRORD_OPERATOR_API_URL`. |
| `HTTP control plane returned 401 Unauthorized`, and the Operator logs no registration | `MIRRORD_OPERATOR_API_URL` isn't set on the task, so the bootstrap registers with the MetalBear Cloud sessions-manager instead ([Add the connection variables](#add-the-connection-variables)). |
| `MIRRORD_OPERATOR_API_CA_DATA is required when MIRRORD_OPERATOR_EKS_CLUSTER_NAME is set` | A [connection variable](#add-the-connection-variables) is missing on the task. |
| `no AWS region to sign the EKS token for` | `MIRRORD_OPERATOR_API_URL` isn't an EKS endpoint hostname, and neither `AWS_REGION` nor `AWS_DEFAULT_REGION` is set. Set `AWS_REGION` to the cluster's region. |
| `No AWS credentials provider found in environment` | The task definition has no `taskRoleArn`. |
| `Initial AWS credential resolution failed, will retry` | The task role's credentials weren't available at startup. The bootstrap retries a few times with backoff, then stops with an error. |
| `Token refresh failed, will retry` | Fetching the task role's credentials failed; the current token stays in use while it retries. If it still fails when the current token expires, the bootstrap stops with an error. |

To check name resolution and reachability from inside a running task, use [ECS Exec](https://docs.aws.amazon.com/AmazonECS/latest/developerguide/ecs-exec.html) if it's enabled for the service. The endpoint hostname must resolve to addresses within the EKS VPC CIDR, and TCP 443 on it must be reachable.

## Reference documentation

* [EKS access entries](https://docs.aws.amazon.com/eks/latest/userguide/access-entries.html)
* [EKS cluster endpoint access control](https://docs.aws.amazon.com/eks/latest/userguide/cluster-endpoint.html)
* [Route 53 Resolver forwarding rules](https://docs.aws.amazon.com/Route53/latest/DeveloperGuide/resolver-forwarding-outbound-queries.html)
* [Security](../../managing-mirrord/security.md#how-do-ecs-workloads-authenticate-to-the-operator)
