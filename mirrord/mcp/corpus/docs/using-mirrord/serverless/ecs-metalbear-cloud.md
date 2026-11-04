---
title: "Connecting ECS to MetalBear Cloud"
description: "Connect an Amazon ECS task to the MetalBear Cloud sessions-manager: store the API key in AWS Secrets Manager, allow outbound HTTPS, and add the connection variables to the task."
tags:
  - experimental
  - enterprise
---

# Connecting ECS to MetalBear Cloud

{% hint style="warning" %}
Connecting ECS to MetalBear Cloud is **experimental**, and may change without notice.
{% endhint %}

This guide connects an ECS task that runs the [mirrord remote bootstrap](ecs.md) to the [MetalBear Cloud sessions-manager](metalbear-cloud.md). The task authenticates with your MetalBear API key, which ECS injects from AWS Secrets Manager. No Kubernetes cluster, mirrord Operator or IAM permissions on the task role are needed. At a glance, the setup is:

1. Store the API key in AWS Secrets Manager, and let the task's execution role read it ([Store the API key](#store-the-api-key)).
2. Allow outbound HTTPS from the tasks to MetalBear ([Allow outbound HTTPS](#allow-outbound-https)).
3. Add the connection variables to the task and deploy it ([Add the connection variables](#add-the-connection-variables)).
4. [Verify](#verify) that the task registers and a developer session works.

## Prerequisites

1. The ECS task definition set up with the [remote bootstrap](ecs.md), up to [Connect to your deployment mode](ecs.md#connect-to-your-deployment-mode).
2. An [API key](README.md#api-key), `<API_KEY>`.
3. An IAM **execution role** on the ECS service (`executionRoleArn` in the task definition). This is not the task role.
4. The AWS CLI, with permission to manage Secrets Manager secrets, IAM role policies and the ECS task definition.

Throughout this guide, replace:

| Placeholder | Value |
| --- | --- |
| `<REGION>` | The AWS region the ECS service runs in |
| `<EXECUTION_ROLE_NAME>` | The name of the ECS service's execution role |
| `<EXECUTION_ROLE_ARN>` | The ARN of that execution role |
| `<SECRET_ARN>` | The ARN of the secret created in [Store the API key](#store-the-api-key) |

---

## Store the API key

Create a secret holding the API key as plain text:

```bash
aws secretsmanager create-secret --region <REGION> \
  --name mirrord/sessions-manager-api-key \
  --secret-string '<API_KEY>' \
  --query ARN --output text
```

The command prints the secret's ARN, `<SECRET_ARN>`. One secret serves every ECS service in the region.

ECS reads the secret with the task's **execution role** when the task starts, and passes it to the container as an environment variable. Allow the execution role to read it:

```bash
aws iam put-role-policy --role-name <EXECUTION_ROLE_NAME> \
  --policy-name mirrord-sessions-manager-api-key \
  --policy-document '{
    "Version": "2012-10-17",
    "Statement": [{
      "Effect": "Allow",
      "Action": "secretsmanager:GetSecretValue",
      "Resource": "<SECRET_ARN>"
    }]
  }'
```

If the secret is encrypted with a customer-managed KMS key rather than the default `aws/secretsmanager` key, also allow `kms:Decrypt` on that key.

## Allow outbound HTTPS

The tasks only make outbound connections, to the hosts in [Network access](metalbear-cloud.md#network-access). Check that:

* The tasks' security group allows outbound TCP 443 to them.
* The tasks' subnets route to the internet, through a NAT gateway for private subnets, or have a public IP for public ones.
* Any firewall or egress proxy on the path allows `app.metalbear.com` and `sm-aws-use1.app.metalbear.com` (or `*.app.metalbear.com`, see [Network access](metalbear-cloud.md#network-access)), doesn't buffer responses, and has an idle timeout of at least 300 seconds.

## Add the connection variables

Add the API key to the application container's `secrets`, next to the `environment` variables from [Amazon ECS](ecs.md#name-the-workload):

```json
{
  "executionRoleArn": "<EXECUTION_ROLE_ARN>",
  "containerDefinitions": [
    {
      "name": "app",
      "secrets": [
        { "name": "MIRRORD_SESSIONS_MANAGER_API_KEY", "valueFrom": "<SECRET_ARN>" }
      ]
    }
  ]
}
```

| Variable | Value |
| --- | --- |
| `MIRRORD_SESSIONS_MANAGER_API_KEY` | Injected by ECS from `<SECRET_ARN>`. Don't put the key in `environment` or in an environment file, where it's visible to anyone who can describe the task definition. |

{% hint style="warning" %}
Don't set `MIRRORD_OPERATOR_API_URL` or `MIRRORD_OPERATOR_KUBE_CONTEXT` on the task. They select the Operator-hosted mode instead.
{% endhint %}

Register the new task definition revision and update the service to use it. Tasks read the secret when they start, so after [rotating the key](metalbear-cloud.md#api-keys), update the secret and redeploy the services.

---

## Verify

1. **ECS task registered**: after the new task starts, the application container's logs show no sessions-manager or API key errors. With `MIRRORD_AGENT_RUST_LOG=info` in the container's `environment`, they also show `Registering with the MetalBear Cloud sessions-manager`.
2. **Developer session**: with the [developer setup](metalbear-cloud.md#developer-setup) and the `mirrord.json` from [Using mirrord with a Serverless Workload](README.md#using-mirrord-with-a-serverless-workload), without `"operator": true`, `mirrord exec` connects, and a request with the `baggage` header reaches the local process.

## Infrastructure as code

If you manage your infrastructure with Terraform, each step maps to standard resources:

| Step | Terraform resources |
| --- | --- |
| Store the API key | `aws_secretsmanager_secret`, `aws_secretsmanager_secret_version`, `aws_iam_role_policy` |
| Allow outbound HTTPS | `aws_vpc_security_group_egress_rule` |
| Connection variables | `aws_ecs_task_definition` |

## Troubleshooting

The bootstrap's messages appear in the application container's logs. For problems that don't depend on the platform, see [MetalBear Cloud Sessions-Manager](metalbear-cloud.md#troubleshooting), and for ones that don't depend on the deployment mode, [Amazon ECS](ecs.md#troubleshooting).

| Symptom | Likely cause |
| --- | --- |
| The task fails to start with `ResourceInitializationError: unable to pull secrets` | The execution role can't read the secret, or can't decrypt it with its KMS key ([Store the API key](#store-the-api-key)), or the task has no route to Secrets Manager. |
| `MIRRORD_SESSIONS_MANAGER_API_KEY must hold a MetalBear API key` | The secret holds something other than the key, such as a JSON object. Store the key as plain text. |
| A connection timeout to sessions-manager or `app.metalbear.com` | Outbound HTTPS is blocked ([Allow outbound HTTPS](#allow-outbound-https)). |
