---
title: IAM Authentication
description: Use AWS RDS or GCP Cloud SQL IAM authentication with mirrord DB branching
tags:
  - beta
  - team
  - enterprise
---

mirrord supports IAM authentication for **AWS RDS** and **GCP Cloud SQL**. Credentials come from the **target pod**: either from its environment, just like connection URLs, or from its cloud identity (IRSA (min version **operator:3.186.0**) / EKS Pod Identity on AWS, Workload Identity on GCP). For general concepts and the full list of config fields, see the [DB Branching overview](../db-branching.md).

{% hint style="info" %}
DynamoDB branches also authenticate to the source account with `"iam_auth": { "type": "aws_rds" }` (the `aws_rds` type name is reused across AWS engines). For DynamoDB, `iam_auth` is **required** when `"copy": { "mode": "all" }` is set. See the [DynamoDB page](dynamodb.md).
{% endhint %}

{% hint style="info" %}
**Default environment variables**: If you do not specify custom credential sources, mirrord automatically looks for standard environment variables in the target pod (e.g., `AWS_REGION`, `GOOGLE_APPLICATION_CREDENTIALS`). You only need additional configuration if your pod uses non-standard variable names.
{% endhint %}

## Connecting to the branch

IAM only authenticates against the real cloud database - the branch is a plain database pod the cloud provider knows nothing about, so an IAM token is not a valid password there. For PostgreSQL branches, mirrord solves this by running the branch pod with trust authentication whenever `iam_auth` is set: the branch accepts whatever credentials your application already sends, the IAM token included, so the application connects without any changes. The source database is unaffected, and branches without `iam_auth` keep regular password authentication.

## AWS RDS IAM Authentication

### Minimal Configuration

Uses the standard AWS environment variables already present in the target pod.

```json
{
  "feature": {
    "db_branches": [
      {
        "type": "pg",
        "version": "16",
        "connection": { "url": "DATABASE_URL" },
        "iam_auth": { "type": "aws_rds" }
      }
    ]
  }
}
```

### Credential sources

The branch init container needs AWS credentials to sign the RDS auth token. mirrord supports both ways a pod usually holds them:

- **Static keys**: if the target pod has `AWS_ACCESS_KEY_ID` and `AWS_SECRET_ACCESS_KEY` (and optionally `AWS_SESSION_TOKEN`) in its environment, mirrord copies them to the branch pod.
- **IRSA / EKS Pod Identity**: if the target pod has no static keys, the branch pod runs under the target pod's service account and receives the same IAM role automatically. The minimal configuration above is all you need.

{% hint style="info" %}
With IRSA, the AWS region is often injected at runtime rather than declared in the pod spec, so mirrord may not find it. If branch creation fails with a missing region error, add `AWS_REGION` to the target pod spec, or point `region` at an env var that is declared there.
{% endhint %}

### Default AWS environment variables

mirrord reads the following variables from the **target pod**, not from your local shell.

| Field | Default fallback |
|-------|------------------|
| `region` | `AWS_REGION`, `AWS_DEFAULT_REGION` |
| `access_key_id` | `AWS_ACCESS_KEY_ID` |
| `secret_access_key` | `AWS_SECRET_ACCESS_KEY` |
| `session_token` | `AWS_SESSION_TOKEN` |

### Custom AWS environment variables

Use this only if your pod uses non-standard variable names.

```json
{
  "iam_auth": {
    "type": "aws_rds",
    "region": { "type": "env", "variable": "MY_CUSTOM_REGION" },
    "access_key_id": { "type": "env", "variable": "MY_ACCESS_KEY" },
    "secret_access_key": { "type": "env", "variable": "MY_SECRET_KEY" }
  }
}
```

## GCP Cloud SQL IAM Authentication

### Minimal Configuration

Uses the standard `GOOGLE_APPLICATION_CREDENTIALS` file path from the target pod. If the target pod uses Workload Identity instead of a credentials file, the same configuration works: the branch pod runs under the target pod's service account and receives the same GCP identity, so no credential fields are needed.

```json
{
  "feature": {
    "db_branches": [
      {
        "type": "pg",
        "version": "17",
        "connection": { "url": "DATABASE_URL" },
        "iam_auth": { "type": "gcp_cloud_sql" }
      }
    ]
  }
}
```

#### Default GCP environment variables

| Field | Default fallback |
|-------|------------------|
| `credentials_path` | `GOOGLE_APPLICATION_CREDENTIALS` |
| `project` | `GOOGLE_CLOUD_PROJECT`, `GCP_PROJECT`, `GCLOUD_PROJECT` |

### Custom GCP credentials

You can override the default behavior in one of the following ways.

**Option 1: Inline JSON credentials**
Load the service account JSON directly from an environment variable.

```json
{
  "iam_auth": {
    "type": "gcp_cloud_sql",
    "credentials_json": { "type": "env", "variable": "GOOGLE_APPLICATION_CREDENTIALS_JSON" }
  }
}
```

**Option 2: Custom credential file path**

Read credentials from a file path provided via an environment variable (for example, a mounted Kubernetes Secret).

```json
{
  "iam_auth": {
    "type": "gcp_cloud_sql",
    "credentials_path": { "type": "env", "variable": "MY_GCP_CREDENTIALS_PATH" }
  }
}
```

{% hint style="warning" %}
Use either `credentials_json` OR `credentials_path`, not both.
{% endhint %}

#### Required Database Settings

GCP Cloud SQL requires TLS.
Make sure your `DATABASE_URL` includes: `sslmode=require`

The `sslmode=require` only applies to the source. The branch connection your application receives carries the branch pod's own TLS mode (`sslmode=disable` for a regular branch pod), so the app does not demand TLS the branch cannot serve. See [Branch Query Parameters](connection.md#branch-query-parameters-postgresql) to override it.
