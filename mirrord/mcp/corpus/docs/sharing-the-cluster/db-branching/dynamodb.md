---
title: DynamoDB
description: Spin up an isolated DynamoDB branch of your remote tables with mirrord
tags:
  - alpha
  - team
  - enterprise
---

This page covers DB branching for DynamoDB. For the general concepts, the full list of config fields, and how a session behaves, see the [DB Branching overview](../db-branching.md).

The branch runs as a [DynamoDB Local](https://docs.aws.amazon.com/amazondynamodb/latest/developerguide/DynamoDBLocal.html) pod (`amazon/dynamodb-local`) in your cluster, so your application talks to an isolated endpoint instead of the source AWS account.

{% hint style="info" %}
DynamoDB branching requires operator `3.179.0`, mirrord CLI `3.228.0`, and operator Helm chart `3.179.0` with the `operator.dynamodbBranching` value set to `true`.
{% endhint %}

## Basic Configuration

```json
{
  "feature": {
    "db_branches": [
      {
        "id": "users-dynamodb",
        "type": "dynamodb",
        "version": "latest",
        "iam_auth": { "type": "aws_rds" },
        "copy": {
          "mode": "all"
        }
      }
    ]
  }
}
```

### Connection

Unlike the other engines, `connection` is optional for DynamoDB. Since there is no user or password, it is only used to point the source client at a custom/VPC endpoint URL (for example `AWS_ENDPOINT_URL_DYNAMODB`); if omitted, the standard regional AWS endpoint is used. See [Connection Modes](connection.md) for the supported source formats.

### IAM Authentication

DynamoDB branches authenticate to the source account with `"iam_auth": { "type": "aws_rds" }` (the `aws_rds` type name is reused across AWS engines). Since DynamoDB has no password-based auth, `iam_auth` is **required** when `"copy": { "mode": "all" }` is set. Credentials are read from the target pod's environment - see [IAM Authentication](iam-authentication.md) for details and custom credential sources.

## Copy Modes

DynamoDB supports two copy modes. Unlike the SQL engines, there is no `"schema"` mode - tables are schema-on-write, so only `"empty"` and `"all"` are available.

| Mode | What gets cloned | Best for |
| --- | --- | --- |
| `"empty"` (default) | Nothing - an empty branch with no tables copied | Workflows where your application creates its own tables or runs migrations/seeding as part of startup |
| `"all"` | The schema and items of every source table | A full clone of your environment data for debugging or reproducing production-like scenarios |

{% hint style="info" %}
`"mode": "all"` requires [`iam_auth`](#iam-authentication), since reading the source account is only possible through AWS IAM.
{% endhint %}

{% hint style="warning" %}
Use `"mode": "all"` with caution.
It's only recommended for very small or empty tables.
Copying large datasets can significantly increase branch creation time and storage usage.
{% endhint %}

### Table Filters

You can restrict which tables are copied and apply a per-table filter using the `collections` map. A `filter` is a DynamoDB [`FilterExpression`](https://docs.aws.amazon.com/amazondynamodb/latest/developerguide/Query.FilterExpression.html) applied during the `Scan` of the source table.

```json
{
  "copy": {
    "mode": "all",
    "collections": {
      "users": {
        "filter": "active = true"
      },
      "orders": {}
    }
  }
}
```

In this example, only the `users` and `orders` tables are copied. The `users` table copy includes only items where `active = true`; the `orders` table is copied in full.

## Known Limitations

{% hint style="warning" %}
- **Filter expressions cannot define `ExpressionAttributeValues` or `ExpressionAttributeNames`.** The `filter` string is passed straight to `Scan`, so expressions that rely on placeholders (for example `age > :min` or `#n = "alice"`) fail at runtime. Only self-contained expressions work.
- **Local Secondary Indexes (LSIs) are not copied.** Only Global Secondary Indexes (GSIs) are reconstructed on the branch, because LSIs can only be created at table-creation time.
- **Branch tables always use PayPerRequest billing**, regardless of the source table's billing mode.
{% endhint %}
