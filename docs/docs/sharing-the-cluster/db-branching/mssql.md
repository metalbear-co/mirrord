---
title: MSSQL
description: Spin up an isolated Microsoft SQL Server branch of your remote database with mirrord
tags:
  - beta
  - team
  - enterprise
---

# MSSQL

This page covers DB branching for Microsoft SQL Server (MSSQL). For the general concepts, the full list of config fields, and how a session behaves, see the [DB Branching overview](../db-branching.md).

{% hint style="info" %}
MSSQL branching requires operator `3.150.0`, mirrord CLI `3.195.0`, and operator Helm chart `1.57.0` with the `operator.mssqlBranching` value set to `true`.
{% endhint %}

{% hint style="warning" %}
SQL Server images refuse to start until their license is accepted, so the operator starts every MSSQL branch database with `ACCEPT_EULA=Y`. Setting `operator.mssqlBranching` to `true` means you accept Microsoft's license terms for the SQL Server edition the branch image runs. This holds for any image a branch uses: the default `mcr.microsoft.com/mssql/server`, a registry set in `mssqlBranchConfig.dbPod.image`, a [profile](../db-branching.md#branch-config-profiles), or a [per-branch `image`](../db-branching.md#custom-branch-image). The default image runs SQL Server Developer, which is licensed for non-production use only. Read the [SQL Server Developer license terms](https://go.microsoft.com/fwlink/?linkid=857698) before you enable it.
{% endhint %}

## Basic Configuration

```json
{
  "feature": {
    "db_branches": [
      {
        "id": "users-mssql-db",
        "type": "mssql",
        "version": "2022-latest",
        "name": "users-database-name",
        "connection": {
          "url": "DATABASE_URL"
        },
        "copy": {
          "mode": "empty"
        }
      }
    ]
  }
}
```

The `connection` field describes how mirrord locates the source database connection details - a full connection URL or individual parameters (host, port, user, password, database). See [Connection Modes](connection.md) for all supported sources, including Kubernetes Secrets, Google Secret Manager, literal values, and composite environment variables.

## Copy Modes

The `copy` field controls what data gets cloned when creating an MSSQL branch.

| Mode | What gets cloned | Best for |
| --- | --- | --- |
| `"empty"` (default) | Nothing - an empty database with no schema or data | Workflows where your application initializes the schema or runs migrations as part of startup |
| `"schema"` | Only the table structures (schemas) from the source database, without any data | Testing schema changes or local development where structure is needed but data is not |
| `"all"` | Everything from the source database - both schema and data | A full clone of your environment data for debugging or reproducing production-like scenarios |

{% hint style="warning" %}
Use `"mode": "all"` with caution.
It’s only recommended for very small or empty databases.
Copying large datasets can significantly increase branch creation time and storage usage.
{% endhint %}

### Filtered Data Clone

Developers can customize what gets copied per table. This allows copying only specific rows or subsets of data using SQL query filters.

```Json
{
  "copy": {
    "mode": "schema",                   // Or "empty" as explained below
    "tables": {
      "users": {
        "filter": "name = 'alice' OR name = 'bob'"
      },
      "orders": {
        "filter": "created_at > 1759948761"
      }
    }
  }
}
```

#### In this example

The schema for all tables is cloned.
The `users` table copy includes only rows for `alice` and `bob`.
The `orders` table copy includes only rows created after a certain timestamp.

Filtering can also be combined with `"mode": "empty"`, in which case only the specified tables (and their filtered data) are copied, while all others are excluded.

Note: Filtering is not compatible with `"mode": "all"`.
If both are specified, mirrord ignores the `tables` configuration.

{% hint style="info" %}
The `dump_args` field is not supported for MSSQL. MSSQL branches use their own internal dump mechanism, so only MySQL, MariaDB, and PostgreSQL branches accept custom dump arguments.
{% endhint %}
