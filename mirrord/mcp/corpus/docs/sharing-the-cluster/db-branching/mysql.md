---
title: MySQL
description: Spin up an isolated MySQL branch of your remote database with mirrord
tags:
  - beta
  - team
  - enterprise
---

This page covers DB branching for MySQL. For the general concepts, the full list of config fields, and how a session behaves, see the [DB Branching overview](../db-branching.md).

{% hint style="info" %}
MySQL branching requires operator `3.129.0`, mirrord CLI `3.160.0`, and operator Helm chart `1.37.0` with the `operator.mysqlBranching` value set to `true`.
{% endhint %}

## Basic Configuration

```json
{
  "feature": {
    "db_branches": [
      {
        "id": "users-mysql-db",
        "type": "mysql",
        "version": "8.0",
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

The `copy` field controls what data gets cloned when creating a MySQL branch.

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

### What the copy carries over

Everything in this section requires mirrord operator `3.210.0` or later. Earlier operators copy tables and data only, and start the branch server on the image's own settings.

In `schema` and `all` modes the branch gets the source database's views, triggers, stored functions and stored procedures along with its tables. Every copied object is owned by the branch's `root` user: the `DEFINER` the source recorded is dropped, since that account does not exist on the branch and an object that kept it would fail with `The user specified as a definer does not exist`.

The copy runs `mysqldump` as the declared connection user. The server only shows a routine's body to its definer, to an account with `SHOW_ROUTINE` (MySQL 8.0.20 and later) or to one with the global `SELECT` privilege, so routines the connection user defined itself always come along, and routines defined by other accounts need one of those grants; without it `mysqldump` leaves them out with an `insufficient privileges` comment in place of the body. `EXECUTE` alone is not enough.

Regardless of the copy mode, the branch server starts with the source server's `sql_mode`, `character_set_server`, `collation_server`, `time_zone`, `group_concat_max_len`, `explicit_defaults_for_timestamp` and transaction isolation, read from the source when the branch is created. Values from the cluster admin's `dbServerArgs` still take precedence. The branch server must accept the source's values, so keep the branch `version` on the same major version as the source: an `sql_mode` flag one version removed stops the other from starting.

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

## Custom Dump Arguments

The `dump_args` field lets you customize the arguments passed to `mysqldump`, the tool mirrord uses to copy the source database. It is available in all three copy modes (`empty`, `schema`, and `all`).

By default, mirrord passes no arguments to `mysqldump`, which then runs with its own built-in defaults (the [`--opt`](https://dev.mysql.com/doc/refman/8.4/en/mysqldump.html#option_mysqldump_opt) option group). Arguments listed in `dump_args` are passed to the tool as-is. An empty list (`[]`) removes `mysqldump`'s built-in defaults.

### Example - single transaction and no table locking

```json
{
  "copy": {
    "mode": "all",
    "dump_args": ["--single-transaction", "--no-tablespaces", "--skip-lock-tables"]
  }
}
```

In this example, `mysqldump` runs with `--single-transaction`, `--no-tablespaces`, and `--skip-lock-tables`.

## IAM Authentication

MySQL branches can authenticate to the source database with IAM instead of a password, on both **AWS RDS** and **GCP Cloud SQL**. See [IAM Authentication](iam-authentication.md) for setup and examples.
