---
title: Schema Migrations
description: Automatically run schema migrations against a mirrord DB branch
tags:
  - alpha
  - team
  - enterprise
---

A DB branch includes the source database's schema as of when it was cloned - or starts with no schema at all, depending on `copy.mode`. Either way, it won't include migrations that exist only in your working tree and haven't been applied to the source database yet.

The `migrations` field closes that gap: mirrord runs your schema migrations against the branch automatically when it's created, so the branch comes up with the schema your code expects.

This page covers the `migrations` field of a DB branch config for MySQL, MariaDB, PostgreSQL, CockroachDB, and MSSQL. For general concepts and the full list of config fields, see the [DB Branching overview](../db-branching.md).

{% hint style="info" %}
Minimum versions per capability:

| Capability | mirrord CLI | Operator | Helm chart |
| --- | --- | --- | --- |
| Schema migrations (Flyway, local `path`) | `3.230.0` | `3.182.0` | `3.182.0` |
| Migrations on MariaDB branches | `3.235.0` | `3.186.0` | `3.186.0` |
| Image-native Flyway (`locations`), `container` flavor | `3.238.0` | `3.187.0` | `3.187.0` |
| Inherited target environment (`container` flavor) | `3.238.0` | `3.191.0` | `3.191.0` |
| Liquibase migrations (`liquibase` flavor) | `3.257.0` | `3.207.0` | `3.207.0` |
{% endhint %}

## How it works

When your session starts a branch whose config has `migrations`:

1. The operator creates the branch database pod and clones the source schema/data into it, per `copy.mode`. Migrations are not involved in this step.
2. Once the branch database accepts connections, the operator runs your migrations as a one-shot Kubernetes Job next to it. The Job connects to the branch - never to the source database - using the branch's own admin credentials.
3. The branch becomes ready only after the Job succeeds, and your session starts against the fully migrated branch. If the Job fails, your session aborts and mirrord prints the Job's log output, so your app never runs against a half-migrated schema.

When a session reuses an existing branch with a changed migration set, the operator runs a new Job for the delta. A migration that conflicts with one already applied (for example, an edited already-applied Flyway file or Liquibase changeset) fails your session only; the branch stays usable for other sessions.

The `flavor` field selects what the Job runs. Pick by where your migrations live:

| Your migrations are... | Use |
| --- | --- |
| Flyway SQL files in the repository you're working in | `flavor: flyway` with `path` |
| Flyway SQL files baked into an image your CI publishes | `flavor: flyway` with `image` + `locations` |
| Liquibase changelogs in the repository you're working in | `flavor: liquibase` with `path` |
| Liquibase changelogs baked into an image your CI publishes | `flavor: liquibase` with `image` + `search_path` |
| Part of your app's own tooling (a migration script or framework CLI baked into the image) | `flavor: container` |

Using `migrations` requires the branch's `name` field to be set.

The branch's `creation_timeout_secs` covers the whole startup: cloning the source, pulling the migration image, and running the migrations. Raise it if your migrations are slow or your migration image is large.

## Carrying migration history onto the branch

`"copy": { "mode": "schema" }` copies table definitions and no rows—including the table (or tables) your migration tool records applied migrations in.

To carry the migration history onto the branch, name the history table under `tables` so its rows are copied along with its definition:

```json
{
  "copy": {
    "mode": "schema",
    "tables": {
      "flyway_schema_history": {}
    }
  }
}
```

Liquibase keeps two tables, and both have to come across:

```json
{
  "copy": {
    "mode": "schema",
    "tables": {
      "DATABASECHANGELOG": {},
      "DATABASECHANGELOGLOCK": {}
    }
  }
}
```

## Flyway migrations

Set `"flavor": "flyway"` to run versioned SQL files (`V1__create_users.sql`, `V2__add_orders.sql`, ...) with [Flyway](https://documentation.red-gate.com/flyway). Flyway records applied migrations in a `flyway_schema_history` table inside the branch, so re-runs apply only what's new.

Mind the interaction with `copy.mode`: Flyway refuses to migrate a schema that already has objects but no `flyway_schema_history` table. If your source database isn't itself Flyway-managed, use `"copy": { "mode": "empty" }` and let the migrations build the branch schema from scratch. If the source is Flyway-managed, `all` brings the history table's rows across with everything else and the Job applies only your newer files, and `schema` needs [special configuration](#carrying-migration-history-onto-the-branch).

| Field | Description |
| --- | --- |
| `path` | Local directory of migration files, resolved relative to your working directory. Mutually exclusive with `locations`. |
| `locations` | Flyway locations inside `image` holding the migration files, for images with the SQL baked in. Mutually exclusive with `path`, and requires `image`. |
| `image` | Migration runner image. Optional with `path` (defaults to `flyway/flyway:12`), required with `locations`. |

Exactly one of `path` or `locations` is required.

### From a local directory

With `path`, mirrord uploads the directory (up to 1 MiB compressed) and the Job runs the stock Flyway image against it. This is the fit when the migration files live in the repository you're working in:

```json
{
  "feature": {
    "db_branches": [
      {
        "type": "pg",
        "version": "17",
        "name": "users-database-name",
        "connection": { "url": "DATABASE_URL" },
        "migrations": {
          "flavor": "flyway",
          "path": "./migrations"
        }
      }
    ]
  }
}
```

### From a migration image

With `locations`, nothing is uploaded: the SQL is already baked into a migration image (typically published by CI), and the Job runs that image with Flyway pointed at the in-image paths. This is the fit when your deployment pipeline already migrates the shared database by running such an image, and previews should run exactly the same artifact:

```json
{
  "migrations": {
    "flavor": "flyway",
    "image": "registry.example.com/my-migrations:latest",
    "locations": ["filesystem:/flyway/sql"]
  }
}
```

`locations` accepts multiple entries, joined into `FLYWAY_LOCATIONS`.

## Liquibase migrations

Set `"flavor": "liquibase"` to run [Liquibase](https://docs.liquibase.com) changelogs, in XML, YAML, JSON or formatted SQL. Liquibase records applied changesets in a `DATABASECHANGELOG` table inside the branch, so re-runs apply only what's new.

Liquibase starts from a single root changelog rather than scanning a directory, so `changelog_file` is required.

| Field | Description |
| --- | --- |
| `changelog_file` | Root changelog file, resolved inside the search root. Recorded in `DATABASECHANGELOG`, so changing it re-runs every changeset. |
| `path` | Local directory of changelog files, resolved relative to your working directory. Mutually exclusive with `search_path`. |
| `search_path` | Liquibase search path inside `image`. Mutually exclusive with `path`, and requires `image`. |
| `image` | Migration runner image. Optional with `path` (defaults to `liquibase/liquibase:4.33`), required with `search_path`. |

Exactly one of `path` or `search_path` is required. Liquibase 4.33 bundles the PostgreSQL, MariaDB and SQL Server drivers; a custom `image` must carry the driver for your dialect, and MySQL branches are reached over `jdbc:mariadb://`.

Mind the interaction with `copy.mode`: if your source database isn't itself Liquibase-managed, use `"copy": { "mode": "empty" }` and let the changelogs build the branch schema from scratch. If the source is Liquibase-managed, `all` brings both history tables across with everything else, and `schema` needs [special configuration](#carrying-migration-history-onto-the-branch).

### From a local directory

With `path`, mirrord uploads the directory (up to 1 MiB compressed) and the Job runs the stock Liquibase image against it. `changelog_file` names the root changelog inside that directory, and `include` / `includeAll` paths resolve against it:

```json
{
  "feature": {
    "db_branches": [
      {
        "type": "pg",
        "version": "17",
        "name": "users-database-name",
        "connection": { "url": "DATABASE_URL" },
        "migrations": {
          "flavor": "liquibase",
          "path": "./changelog",
          "changelog_file": "db.changelog-master.xml"
        }
      }
    ]
  }
}
```

### From a migration image

With `search_path`, nothing is uploaded: the changelogs are already baked into an image, and the Job runs that image with Liquibase pointed at the in-image paths:

```json
{
  "migrations": {
    "flavor": "liquibase",
    "image": "registry.example.com/my-migrations:latest",
    "search_path": ["/liquibase/changelog"],
    "changelog_file": "db.changelog-master.xml"
  }
}
```

`search_path` accepts multiple entries, joined into `LIQUIBASE_SEARCH_PATH`; Liquibase looks for `changelog_file` under each in turn.

## Container migrations

Set `"flavor": "container"` to run your own image as the migration Job. This is the fit when migrations aren't standalone SQL files but part of your application's own tooling - for example an app image whose setup script runs the framework's migration command:

```json
{
  "migrations": {
    "flavor": "container",
    "image": "registry.example.com/my-app:latest",
    "command": ["bundle", "exec", "rake", "db:migrate"]
  }
}
```

| Field | Description |
| --- | --- |
| `image` | Full image reference for the migration container, including the tag. |
| `command` | *(Optional)* Entrypoint command override. When unset, the image's own entrypoint runs. |
| `args` | *(Optional)* Entrypoint args override. |
| `env` | *(Optional)* Extra environment variables for the container. Entries here override inherited values of the same name. |

The container must exit 0 when migrations succeed; any other exit code fails the Job (and your session) with the container's logs as the error.

`command` and `args` are the Kubernetes container `command`/`args`, so anything a pod spec can run works here - as long as it exists inside the image (the container flavor uploads nothing from your machine). Common shapes:

```json
"command": ["/app/bin/migrate"]
```

```json
"command": ["sh", "-c", "/app/bin/migrate && /app/bin/seed"]
```

Chain multiple scripts with `&&` so the first failure stops the chain and fails the Job.

### The Job runs with your app's environment

The Job inherits the target container's `env` and `envFrom` (ConfigMaps and Secrets) automatically. This matters when the migration command boots your whole application - a Rails `rake db:migrate`, a Django `manage.py migrate` - and the app needs its full configuration to start: the Job gets the same environment as the app's own pods, with nothing copied into `migrations.env`. Secret-backed values stay Kubernetes refs; the operator never reads them.

The inherited environment includes your app's real database connection values, so the operator redirects them: every variable named in the branch's `connection` is set to the branch's value on the Job, taking precedence over the inherited one. Your migration tool reads its usual variable (`DATABASE_URL` for the Rails example above) and lands on the branch - the source database stays out of reach through every variable mirrord knows about.

If the operator cannot tell which variables carry the connection - a `connection` declared through a `secret`, `gcp_secret_manager`, or `aws_secrets_manager` source without `env_var_name` - the migration fails with an error rather than run with the source connection in the environment. Add `env_var_name` to the source, declare env-based connection params, or have the cluster admin disable `migrationEnv.inherit`.

On multi-container targets, the Job inherits from the container your target path names (`deployment/app/container/main`). Without a named container, it uses the container that defines one of the declared connection variables, falling back to the first container - so a sidecar listed ahead of your app doesn't donate its environment.

The Job's environment merges five layers; when the same name appears in several, the winner is, strongest first: your `migrations.env`, the injected `MIRRORD_DB_*`, the branch-redirected connection variables, the admin's `migrationEnv`, the inherited target environment. So a `migrations.env` entry is the escape hatch for any inherited or admin value that shouldn't apply to migrations (for example forcing `RAILS_ENV`).

### Admin control over the Job's environment

Cluster admins tune this behavior per database type - and per branch-config profile - with `migrationEnv` in the operator's Helm values:

```yaml
mysqlBranchConfig:
  dbPod:
    migrationEnv:
      inherit: true             # the default; set to false to give the Job only MIRRORD_DB_* and migrations.env
      env:
        - name: RAILS_ENV
          value: development
        - name: API_KEY
          valueFrom:
            secretKeyRef:
              name: migration-secrets
              key: api-key
      envFrom:
        - configMapRef:
            name: shared-migration-config
```

| Field | Description |
| --- | --- |
| `inherit` | Whether the Job inherits the target container's `env`/`envFrom`. Defaults to `true`. |
| `env` | Extra variables for every migration Job, in the plain Kubernetes `env` shape - `valueFrom` works, so secret values stay in-cluster. Entries override inherited values of the same name, but never the branch-redirected connection variables. |
| `envFrom` | Extra ConfigMap/Secret refs, appended after the target's own so their keys win on collisions. |

This is useful for values every migration needs but the target doesn't carry, or - with `inherit: false` - for handing the Job a fully admin-curated environment instead of the app's.

### Reaching the branch from your container

The operator injects the branch connection into the container as environment variables:

| Variable | Value |
| --- | --- |
| `MIRRORD_DB_HOST` | Branch database host. |
| `MIRRORD_DB_PORT` | Branch database port. |
| `MIRRORD_DB_USER` | The branch engine's default admin user. |
| `MIRRORD_DB_PASSWORD` | The branch's root password. Omitted when the branch has none. |
| `MIRRORD_DB_NAME` | The branch database name. |

Most tools don't need them: the app's usual connection variables are inherited and already redirected to the branch. The `MIRRORD_DB_*` vars are for explicit wiring when that's not enough:

- **Compose variables your tool reads**, with Kubernetes [`$(VAR)` expansion](https://kubernetes.io/docs/tasks/inject-data-application/define-interdependent-environment-variables/) in `env`, `command`, or `args` - for example `"env": { "DATABASE_URL": "mysql://$(MIRRORD_DB_USER):$(MIRRORD_DB_PASSWORD)@$(MIRRORD_DB_HOST):$(MIRRORD_DB_PORT)/$(MIRRORD_DB_NAME)" }`. This needs no changes to the image.
- **Read them from the environment in your script**: `psql -h "$MIRRORD_DB_HOST" ...` or an updated `db_setup.sh` that prefers `MIRRORD_DB_*` when present.

Note that these are the *branch's* credentials, deliberately not your source database's: the Job needs admin rights on the branch, and it must never touch the shared database.

### Composing the config in CI

A preview pipeline usually decides the image tag and the command when it prepares the config - what mirrord ultimately receives is a plain config like:

```json
{
  "migrations": {
    "flavor": "container",
    "image": "registry.example.com/my-app:build-1234",
    "command": ["bin/migrate"]
  }
}
```

How the pipeline fills those values in is up to it; mirrord config files also support [Tera templating](https://metalbear.com/mirrord/docs/config) for reading them from the environment in place.

## When migrations fail

A failed migration aborts your session, and the error mirrord prints is the Job's own log output - a Flyway error, or whatever your container wrote before exiting non-zero. To dig deeper in the cluster:

- The branch resource records the outcome: `kubectl get branchdatabases -n <namespace>` and look at `status.migrations` (phase, and the error text on failure).
- The Jobs are named `mirrord-migrations-<branch-uid>-<generation>`; `kubectl logs job/<name>` shows the full run, including successful ones.

Failures you may hit: a migration with a SQL error (fix the file; a fresh session re-runs it), an edited already-applied Flyway file or Liquibase changeset on a reused branch (checksum conflict - fails your session only), an image rejected by the admin's `allowedImages` list, an image that can't be pulled, a `DATABASECHANGELOGLOCK` row left held by an interrupted Liquibase migration, or a `container` migration whose `connection` variables the operator cannot redirect (see [The Job runs with your app's environment](#the-job-runs-with-your-apps-environment)).

## Restricting migration images

Cluster admins can restrict which images are accepted with the per-database `dbPod.allowedImages` list in the operator's Helm values. The list applies to every migration image a user supplies — the `container` flavor's `image`, and any `migrations.image` override on the `flyway` or `liquibase` flavors, whether the migration files are uploaded or baked into the image — the same way it applies to branch database images. A disallowed image fails the migration with a clear error before anything runs.

The flavor defaults are not matched against the list; `dbPod.migrationImages.<tool>.registry` controls where those come from.
