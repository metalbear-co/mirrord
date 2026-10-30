---
title: MariaDB
description: Spin up an isolated MariaDB branch of your remote database with mirrord
tags:
  - beta
  - team
  - enterprise
---

# MariaDB

This page covers DB branching for MariaDB. For the general concepts, the full list of config fields, and how a session behaves, see the [DB Branching overview](../db-branching.md).

{% hint style="info" %}
MariaDB branching requires operator `3.186.0`, mirrord CLI `3.235.0`, and operator Helm chart `3.186.0` with the `operator.mariadbBranching` value set to `true`.
{% endhint %}

MariaDB is a first-class engine, not a MySQL alias. Branches use MariaDB-native tooling (`mariadb-dump` / `mariadb-admin`) and a MariaDB branch image, so MariaDB-only objects such as sequences and system-versioned (temporal) tables are copied correctly - which a MySQL dump and image mishandle.

## Basic Configuration

```json
{
  "feature": {
    "db_branches": [
      {
        "id": "users-mariadb-db",
        "type": "mariadb",
        "version": "12",
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

MariaDB speaks the MySQL wire protocol, so your application connects to the branch with the same driver it already uses. When mirrord builds the branch connection URL from individual parameters, it uses the `mariadb` scheme.

## Copy Modes

The `copy` field controls what data gets cloned when creating a MariaDB branch.

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

In `schema` and `all` modes the branch gets the source database's views, triggers, stored functions and stored procedures along with its tables. Every copied object is owned by the branch's `root` user: the `DEFINER` the source recorded is dropped, since that account does not exist on the branch and an object that kept it would fail with `The user specified as a definer does not exist`. With [`roles: full`](#roles-permissions-and-credentials), an object keeps a definer the branch recreates.

The copy runs `mariadb-dump` as the declared connection user. The server only shows a routine's body to its definer, to an account with `SHOW CREATE ROUTINE` (MariaDB 11.3 and later) or to one with the global `SELECT` privilege, so routines the connection user defined itself always come along, and routines defined by other accounts need one of those grants; without it `mariadb-dump` leaves them out with an `insufficient privileges` comment in place of the body. `EXECUTE` alone is not enough.

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

The `dump_args` field lets you customize the arguments passed to `mariadb-dump`, the tool mirrord uses to copy the source database. It is available in all three copy modes (`empty`, `schema`, and `all`).

By default, mirrord passes no arguments to `mariadb-dump`, which then runs with its own built-in defaults (the [`--opt`](https://mariadb.com/docs/server/clients-and-utilities/backup-restore-and-import-clients/mariadb-dump#opt) option group). Arguments listed in `dump_args` are passed to the tool as-is and can leave out what the copy mode copies, such as stored routines with `--skip-routines`.

### Example - single transaction and no table locking

```json
{
  "copy": {
    "mode": "all",
    "dump_args": ["--single-transaction", "--no-tablespaces", "--skip-lock-tables"]
  }
}
```

In this example, `mariadb-dump` runs with `--single-transaction`, `--no-tablespaces`, and `--skip-lock-tables`.

## Roles, Permissions, and Credentials

Everything in this section requires mirrord operator `3.220.0` or later.

A branch pod is a fresh MariaDB server, so mirrord recreates the source database's accounts in it. How much of them it recreates is controlled by the operator's Helm values, per cluster:

```yaml
operator:
  mariadbBranchConfig:
    dbPod:
      roles: "empty" # or "full"
```

`empty` (the default) recreates only the user declared in the branch's `connection` config. The copy drops definers, and your app connects through mirrord's env overrides as `root`.

`full` recreates the source accounts with their grants, and copied objects keep a definer the branch recreates. The branch then enforces the same permissions as the source. In `full` mode, mirrord's env overrides only redirect the connection address, so the app keeps using its own user and password (see the table below for how this interacts with `url`-style connections).

`full` recreates every source account when the declared user has `SELECT` on the `mysql` schema, and only the declared user otherwise.

### The source user's password

The user declared in the branch's `connection` config can log into the branch with its real password, in both modes. Only a password hash is written into the branch, never the plaintext.

In `empty` mode this login has every privilege on the branch; in `full` mode it has the user's real grants.

### Which credentials does my app end up using?

| `roles` | Connection config | Where the app gets its credentials | App connects to the branch as |
| --- | --- | --- | --- |
| `empty` (default) | `params` or `url` | env vars | `root`, mirrord's branch password |
| `empty` (default) | `params` or `url` | fetched at runtime (secret manager, Vault, ...) | the declared user, its real password (every privilege on the branch) |
| `full` | `params` | env vars | the declared user, its real password, real permissions (mirrord leaves user/password vars untouched) |
| `full` | `params` | fetched at runtime (secret manager, Vault, ...) | the declared user, its real password, real permissions |
| `full` | `url` | env vars | `root` (the URL var is replaced whole) |
| any | IAM auth | any | `root` (no source login is created) |

### Limits

- Accounts other than the declared user are recreated without a password and cannot log in.
- In `empty` mode, the declared user gets its login only when it authenticates with `mysql_native_password`. In `full` mode, the branch fails when the branch server does not load the declared user's authentication plugin.
- The declared user can log in from any host, without the TLS or X.509 requirement it has on the source.
- Roles granted to a declared user without `SELECT` on the `mysql` schema get only the privileges of its default role.
- Accounts are recreated when a branch is created. Changing the Helm value or rotating a source password affects new branches, not ones already running.
- Accounts the server or a cloud provider owns (`root`, `mariadb.sys`, `rdsadmin`, and similar) are skipped.

## IAM Authentication

MariaDB branches on **AWS RDS** can authenticate to the source database with IAM instead of a password. See [IAM Authentication](iam-authentication.md) for setup and examples. GCP Cloud SQL is not applicable here, as it does not offer a MariaDB engine.
