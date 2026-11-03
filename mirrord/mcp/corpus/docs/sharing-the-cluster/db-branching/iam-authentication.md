---
title: IAM Authentication
description: Use AWS RDS or GCP Cloud SQL IAM authentication with mirrord DB branching
tags:
  - beta
  - team
  - enterprise
---

# IAM Authentication

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

### Apps that build the connection URL at runtime

Some applications never read a connection URL from the environment. At startup they look up the database address in AWS Systems Manager Parameter Store (SSM), sign an IAM token, and build the URL in code. mirrord cannot read SSM, and the application would ignore the branch URL mirrord sets for it anyway.

This works without changing the application or the cluster, as long as the code already has a switch that makes it read a URL from an environment variable, for example a flag for local development:

```json
{
  "feature": {
    "env": { "override": { "USE_SSM": "false" } },
    "db_branches": [
      {
        "type": "pg",
        "version": "17",
        "name": "app",
        "connection": {
          "url": {
            "type": "env",
            "variable": "DATABASE_URL",
            "value": "postgresql://app_user@mydb.abc123.us-east-1.rds.amazonaws.com:5432/app?sslmode=require"
          }
        },
        "iam_auth": { "type": "aws_rds" }
      }
    ]
  }
}
```

- `value` is the source URL the branch copies from. It has no password: with `iam_auth`, the branch init container signs its own IAM token with the target pod's service account. The variable does not need to exist in the pod spec.
- `variable` is the name your local process reads. mirrord sets it to the branch URL.
- `name` is the database name from `value`. The operator never reads the literal URL, so it needs the name to hand your process a branch URL for your database rather than `postgres`. mirrord `3.278.0` and later take it from `value` when you leave `name` out; set it yourself on older CLIs.
- `env.override` flips the switch (`USE_SSM` here; use your application's own flag) on your machine only, so the application reads `DATABASE_URL` instead of building the URL itself. The deployed workload is not touched.

To avoid copying the URL into the config, point `connection.url` at the same parameter your application reads instead, with an [AWS SSM Parameter Store source](connection.md#aws-ssm-parameter-store-source). The branch init container then reads the parameter with the target pod's service account. In that case `name` is required, because mirrord never sees the URL.

When the branch copies data (`copy.mode` `schema` or `all`), use `sslmode=require` in `value`, even when the application uses `verify-full`. The branch init container has no copy of the RDS certificate authority to verify against, so `verify-full` fails the copy. `require` still encrypts it. An empty branch reads nothing from the source, so any `sslmode` works there.

{% hint style="warning" %}
Without the switch, the application keeps building its own URL and connects to the real database, not the branch. Check which database your code reaches before you run migrations through mirrord.
{% endhint %}

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
