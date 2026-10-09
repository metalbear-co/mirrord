# Common Issues

> Source: https://metalbear.com/mirrord/docs/sharing-the-cluster/db-branching/

> **Config nesting:** `db_branches` must live under the top-level `feature` object
> (`{"feature": {"db_branches": [ ... ]}}`). Placed at the top level it is ignored.

## Branch creation times out or is very slow

Often caused by using `"mode": "all"` on a large database. The `all` mode copies the entire database including all data, which can take a long time.

**Solution:** Use `"mode": "schema"` or `"mode": "empty"` instead:

```json
{ "copy": { "mode": "schema" } }
```

If you need specific data, use filtered copying (SQL engines):

```json
{
  "copy": {
    "mode": "schema",
    "tables": { "users": { "filter": "id < 100" } }
  }
}
```

Note: filters are ignored when `"mode": "all"` is set — combine filters with `"empty"` or `"schema"` instead.

Unrecoverable pod failures (e.g. `ImagePullBackOff`, `OOMKilled`) fail the branch immediately with the underlying error rather than waiting for `creation_timeout_secs`.

## Connection fails with SSL/TLS errors

Branch databases disable SSL by default (`sslmode=disable` for PostgreSQL). If your application client is configured to require SSL, the connection will fail.

**Solution:** Configure your application to allow non-SSL connections to the branch, or stop the client from forcing SSL. For PostgreSQL, you can also set `query_params: { "sslmode": "require" }` on the branch config if the branch pod itself is configured for TLS (operator/Helm chart **3.197.0+**, CLI **3.250.0+**). (GCP Cloud SQL IAM is the exception — see below.)

## GCP Cloud SQL connection fails

GCP Cloud SQL with IAM authentication requires TLS on the **source** connection. If the connection URL doesn't include `sslmode=require`, the connection to the source will fail.

**Solution:** Ensure the URL includes the SSL mode parameter:

```
postgresql://user@host:5432/dbname?sslmode=require
```

This only applies to the source — the branch connection your app receives carries the branch pod's own TLS mode (`sslmode=disable` by default), so the app itself does not need to demand TLS. Override that with `query_params` (see above) if the branch needs it too.

## Branch is not being reused between sessions

If you expect a branch to persist and be reused but a new one is created each time, check:

1. The `id` field must be set and identical between sessions.
2. The branch TTL hasn't expired. TTL is counted from when no session is using the branch. Default is 5 minutes and it **caps at 15 minutes**. Set `ttl_secs` or `ttl_mins` (mutually exclusive).

```json
{
  "feature": {
    "db_branches": [
      { "id": "my-stable-branch", "ttl_mins": 15, "type": "pg", "connection": { "url": "DATABASE_URL" } }
    ]
  }
}
```

Note: `id` is ignored for local Redis instances.

## Application connects to wrong database

If your application isn't connecting to the branch, the connection variable(s) in your config likely don't match what the app actually reads.

**Solution:** Verify the exact environment variable(s):

```bash
mirrord exec --target pod/<pod-name> -- env | grep -iE 'database|postgres|mysql|redis|mongo'
```

Then match your config to it:

```json
{ "connection": { "url": "DATABASE_URL" } }
```

For apps that split the connection across variables, use params mode; for a value packed into one variable, use `value_pattern`. See the Connection Modes doc.

## ConfigMap connection param rejected

A `configmap` param source needs a `configmap` (or an admin-set `dbPod.sourceConfigMap` on the branch profile) plus a `key` from one side or the other. If a param omits `configmap` and the profile has no `sourceConfigMap`, or both sides omit `key`, the branch fails with an error naming both places to fix it.

**Solution:** Either set `configmap`/`key` directly on the param, or select a `profile` whose `dbPod.sourceConfigMap` supplies them, and confirm operator/Helm chart **3.205.0+** and CLI **3.256.0+** — older versions reject the field outright.

## AWS RDS IAM authentication fails

mirrord reads AWS credentials from the **target pod's** environment (not your local shell).

**Solution:** Ensure the target pod exposes:
- `AWS_REGION` or `AWS_DEFAULT_REGION`
- `AWS_ACCESS_KEY_ID`
- `AWS_SECRET_ACCESS_KEY`
- `AWS_SESSION_TOKEN` (if using temporary credentials)

With IRSA (IAM Roles for Service Accounts), the pod's service account must be configured correctly. For non-standard variable names, set custom sources under `iam_auth`.

## DynamoDB full copy fails

DynamoDB has no password-based auth. `"copy": { "mode": "all" }` **requires** `iam_auth`.

**Solution:**

```json
{ "type": "dynamodb", "iam_auth": { "type": "aws_rds" }, "copy": { "mode": "all" } }
```

Also note: DynamoDB `filter` strings can't use `ExpressionAttributeValues`/`Names` placeholders; only self-contained expressions work. LSIs are not copied; branch tables use PayPerRequest billing.

## MongoDB branch filter syntax errors

MongoDB uses JSON-based filter syntax, not SQL. Filters must be valid MongoDB query documents as escaped JSON strings, and MongoDB supports only `empty` / `all` (no `schema`).

```json
{
  "copy": {
    "mode": "all",
    "collections": {
      "users": { "filter": "{\"role\": \"admin\"}" },
      "orders": { "filter": "{\"status\": {\"$in\": [\"pending\", \"processing\"]}}" }
    }
  }
}
```

## Schema migrations rejected

`migrations` requires the branch `name` to be set and is only available for MySQL, MariaDB, PostgreSQL, and MSSQL. A migration that conflicts with one already applied to the branch fails your session only; the branch stays usable.

## Flyway/Liquibase refuses to migrate a schema-mode branch

`"copy": { "mode": "schema" }` copies table structures only, not rows — so a branch cloned from a Flyway- or Liquibase-managed source has all the schema's objects but an empty (or missing) history table. Flyway refuses to migrate a schema that already has objects but no `flyway_schema_history` table.

**Solution:** Name the history table(s) under `copy.tables` so their rows are copied along with their definitions:

```json
{ "copy": { "mode": "schema", "tables": { "flyway_schema_history": {} } } }
```

Liquibase keeps two history tables, and both have to come across:

```json
{ "copy": { "mode": "schema", "tables": { "DATABASECHANGELOG": {}, "DATABASECHANGELOGLOCK": {} } } }
```

If the source isn't Flyway/Liquibase-managed to begin with, use `"copy": { "mode": "empty" }` instead and let the migrations build the branch schema from scratch.

## Liquibase migration stuck behind `DATABASECHANGELOGLOCK`

An interrupted Liquibase run can leave a lock row held in `DATABASECHANGELOGLOCK` on a reused branch, which fails every subsequent migration Job until it's cleared. Likewise, an edited already-applied changeset fails with a checksum conflict — the branch stays usable, but only that session fails.

## `container` migration fails: connection variables can't be redirected

`flavor: container` migration Jobs automatically inherit the target container's `env`/`envFrom`, and the operator redirects the branch's `connection` variables inside that inherited environment so the migration lands on the branch. This fails if the `connection` is declared via a `secret`, `gcp_secret_manager`, or `aws_secrets_manager` source **without `env_var_name` set** — the operator then has no variable name to redirect, so it fails the migration rather than silently running it against the source connection.

**Solution:** Set `env_var_name` on that connection source, e.g.:

```json
{
  "connection": {
    "params": {
      "password": { "secret": "rds-credentials", "key": "password", "env_var_name": "DB_PASSWORD" }
    }
  }
}
```

Alternatively, ask the cluster admin to disable `migrationEnv.inherit` on the operator's Helm values for that database type — this is an operator/Helm-side setting (see the mirrord-operator skill for details), not something set in `mirrord.json`.

## Generic branch never becomes ready

A plain TCP readiness probe can pass before the service is actually usable (e.g. before first-boot setup completes).

**Solution:** Use an `http_get` or `exec` probe that proves usability:

```json
{ "readiness": { "type": "http_get", "path": "/health" } }
```

Heavy JVM images (Elasticsearch, Cassandra, Couchbase) can OOM at the 512Mi branch-pod default; an admin must raise `dbPod.resources` in `genericBranchConfig`. If an image isn't in the admin's `allowedImages` glob list, the branch fails immediately naming the image.

## Version compatibility issues

Each engine has minimum operator, CLI, and Helm chart versions, plus a per-engine Helm value that must be enabled (e.g. `operator.mysqlBranching: true`, `operator.pgBranching: true`, `operator.dynamodbBranching: true`, `operator.genericBranching: true`). See the Version Requirements table in `SKILL.md`.

**Check your versions:**

```bash
mirrord --version
kubectl get deployment -n mirrord mirrord-operator -o jsonpath='{.spec.template.spec.containers[0].image}'
```

## Local Redis container fails to start

When using local Redis (`"location": "local"`), the container runtime must be available.

**Solution:** Verify Docker (or your runtime) is running:

```bash
docker info
```

Check the local Redis configuration:

```json
{
  "feature": {
    "db_branches": [
      {
        "type": "redis",
        "location": "local",
        "local": { "runtime": "container", "container_runtime": "docker", "port": 6379 }
      }
    ]
  }
}
```
