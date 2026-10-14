---
title: "DB Branching"
description: "How to use mirrord to spin up an isolated DB branch for safe development and testing DB migrations"
date: 2025-08-31T00:00:00+03:00
lastmod: 2025-08-31T00:00:00+03:00
draft: false
menu:
  docs:
    parent: "using-mirrord"
toc: true
tags: ["team", "enterprise"]
---
{% hint style="info" %}
This feature is available to users on the Team and Enterprise pricing plans.
{% endhint %}

The `db_branches` feature in mirrord lets developers spin up an isolated DB branch that mirrors the remote DB, while running safely in isolation. This allows schema changes, migrations, and experiments without impacting teammates or shared environments.
The supported database engines are listed under [Choose Your Database](#choose-your-database) below.

**When is this useful?**

1. **Running schema migrations safely**  
    Developers can test migrations and schema changes without risking corruption of the remote databases.

2. **Experimenting locally**  
    Developers get a branch of the remote DB connected to their local app automatically - no admin setup required.

3. **Collaborating with teammates**  
    If a branch already exists (with the same ID), mirrord reuses it. Developers can share branches as needed.

4. **Validating AI-generated database changes**
    If your AI coding agents need to generate migrations or schema updates, DB branching provides a safe environment to test those changes without risking the shared environment.

## Choose Your Database

Copy modes, version requirements, [branch location](#branch-location), and engine-specific behavior differ per database.
Pick yours to see the full guide:

| Database | Config `type` | Branch location |
| --- | --- | --- |
| [MySQL](db-branching/mysql.md) | `"mysql"` | Remote (k8s) |
| [MariaDB](db-branching/mariadb.md) | `"mariadb"` | Remote (k8s) |
| [PostgreSQL](db-branching/postgresql.md) | `"pg"` | Remote (k8s) |
| [MSSQL](db-branching/mssql.md) | `"mssql"` | Remote (k8s) |
| [MongoDB](db-branching/mongodb.md) | `"mongodb"` | Remote (k8s) |
| [Redis](db-branching/redis.md) | `"redis"` | Remote (k8s) or local |
| [DynamoDB](db-branching/dynamodb.md) | `"dynamodb"` | Remote (k8s) |
| [ClickHouse](db-branching/clickhouse.md) | `"clickhouse"` | Remote (k8s) |
| [CockroachDB](db-branching/cockroachdb.md) | `"cockroachdb"` | Remote (k8s) |
| [Google Spanner](db-branching/spanner.md) | `"spanner"` | Remote (k8s) |
| [S3](db-branching/s3.md) | `"s3"` | Remote (provider) |
| [turbopuffer](db-branching/turbopuffer.md) | `"turbopuffer"` | Remote (provider) |
| [Generic](db-branching/generic.md) (any other service, using your own image) | `"generic"` | Remote (k8s) |

### Branch Location

DB branches can live in various locations, depending on the exact database engine:

1. **Remote (k8s)**  
    Temporary Pod in the same Kubernetes cluster as the session's target.

2. **Local**  
    Container runtime on the developer's machine.

3. **Remote (provider)**  
    Your account with the database provider, for example your AWS account for Amazon S3.

## Prerequisites

Before you start, make sure you have:  
1. The minimum operator, mirrord CLI, and operator Helm chart versions for your database engine, with the engine's branching value enabled in the chart. The exact versions are listed at the top of each database page above.  
2. Your local application is using environment variables or Kubernetes Secrets to store DB connection strings or individual connection parameters.  
3. mirrord installed and working.  

## Configuring `db_branches`
Developers define branches in their `mirrord.json`:
```json
{
  "feature": {
    "db_branches": [
      {
        "id": "users-mysql-db",             // Optional
        "location": "remote",               // Optional, default is "remote", Available options [remote | local]
        "type": "mysql",                    // See "Choose Your Database" above for supported values
        "version": "8.0",
        "name": "users-database-name",      // Optional
        "ttl_secs": 60,                     // Optional, mutually exclusive with `ttl_mins`
        "creation_timeout_secs": 20,        // Optional, Defaults to 60 if not specified
        "connection": {
          "url": "DATABASE_URL"
        },
        "copy": {
          "mode": "empty"                   // Defaults to "empty" if not specified
        }
      }
    ]
  }
}
```

### Key Fields

| Field | Description |
| --- | --- |
| `id` | When reused, mirrord reattaches to the same branch as long as the time-to-live (TTL) has not expired. This allows multiple sessions to share the same database branch. To prevent accidental reuse of another user's branch, it is recommended to assign a unique value (for example, a UUID) as the identifier. Give two entries of the same type their own `id`s: from mirrord CLI `3.267.0`, entries that would be the same branch make mirrord refuse to start the session. All `generic` entries count as one type. This field is not used for `local` instances and has no effect on branch selection or reuse. |
| `location` | Supported values are `remote` and `local`. The default is `remote`. See the [Choose Your Database](#choose-your-database) table for per-engine support. |
| `type` | The database engine to branch. See the [Choose Your Database](#choose-your-database) table for supported values. |
| `version` | Database engine version, used as the tag on the operator's default image (or the registry an admin configured for this engine). Mutually exclusive with `image`. Does not apply to branches created in your account with the provider. |
| `image` | Full image reference for the branch container, including the tag (for example `registry.example.com/postgresql:15-partman`). Overrides the operator's default image and any admin-configured registry entirely. Mutually exclusive with `version`, since the tag is part of the reference. Cluster admins can restrict which images are accepted - see [Restricting Branch Images](#restricting-branch-images). For `generic` branches the image is required and lives in the same field - see the [Generic](db-branching/generic.md) page. Does not apply to branches created in your account with the provider. |
| `profile` | Name of a branch config profile the cluster admin defined for this engine. Selects which pod settings the branch runs with - image registry, pull secrets, TLS mode, resources. When omitted, the operator's default settings apply. See [Branch Config Profiles](#branch-config-profiles). Does not apply to branches created in your account with the provider. |
| `name` | Remote database name to clone, the override URL uses `name` so the connection URL looks like .../dbname. If name is ommited, the override URL just points to the database server; the application must select the DB manually in that case. For Redis, `name` is the database **index** Redis uses to select a logical database rather than a name, so it must be a valid non-negative number. If omitted, it defaults to index `0`. Does not apply to branches created in your account with the provider. |
| `ttl_secs` / `ttl_mins` | Override for branch time-to-live (TTL), expressed in seconds or minutes. The two fields are mutually exclusive — set whichever is more convenient. The default is 5 minutes. |
| `connection` | Describes how to locate the source database connection details. Supports a full connection URL or individual connection parameters. See [Connection Modes](db-branching/connection.md) for details. For DynamoDB, `connection` is optional and, since there is no user or password, is only used to point the source client at a custom/VPC endpoint URL (for example `AWS_ENDPOINT_URL_DYNAMODB`); if omitted, the standard regional AWS endpoint is used. |
| `copy.mode` | Allows developers to control how the database is cloned when creating a branch. Available modes and filtering options differ per engine - see the Copy Modes section on your [database's page](#choose-your-database). |
| `copy.dump_args` | (MySQL, MariaDB & PostgreSQL only) Customize the arguments passed to `mysqldump`, `mariadb-dump`, or `pg_dump`. See [MySQL](db-branching/mysql.md#custom-dump-arguments), [MariaDB](db-branching/mariadb.md#custom-dump-arguments), or [PostgreSQL](db-branching/postgresql.md#custom-dump-arguments) for details. |
| `creation_timeout_secs` | Override for branch creation timeout, in seconds. If the branch isn't ready within this time, the mirrord session fails and returns a timeout error. Use this field to avoid hanging operations when branch creation takes too long or fails. The default is 60 seconds. Unrecoverable pod failures (for example a bad image reference stuck in `ImagePullBackOff`) fail the branch immediately with the underlying error instead of waiting for this timeout. |
| `iam_auth` | Optional IAM authentication for AWS RDS or GCP Cloud SQL. See [IAM Authentication](db-branching/iam-authentication.md) for details. For DynamoDB, `iam_auth` is **required** when using copy mode `all`, since DynamoDB has no password-based auth. |
| `local.port` | Currently only for Local Redis. Sessions that use the same port share a single local Redis database. When a new session starts on that port, it creates a new database instance that replaces the existing one. |
| `migrations` | (MySQL, MariaDB, CockroachDB, PostgreSQL & MSSQL only) Automatically run schema migrations on the branch so it comes up with the schema your code expects. See [Schema Migrations](db-branching/migrations.md) for details. |
| `additional_databases` | (PostgreSQL only) More databases from the same source server, copied into the same branch pod, each with its own optional `connection` and `copy`. See [Several Databases in One Branch](db-branching/postgresql.md#several-databases-in-one-branch). |

### Custom Branch Image

By default, mirrord runs each branch on the operator's built-in image for that engine (optionally pointed at an admin-configured registry). Setting `image` on a branch overrides that with a full image reference you supply, including the tag:

```json
{
  "feature": {
    "db_branches": [
      {
        "type": "pg",
        "image": "registry.example.com/postgresql:15-partman",
        "connection": {
          "url": "DATABASE_URL"
        }
      }
    ]
  }
}
```

This is useful when your service depends on a database image that differs from the stock one - e.g. a Postgres build with extra extensions. `image` and `version` are mutually exclusive: the tag is already part of the image reference.

The same image is used for the branch's main container and for the init container that seeds it, so it must be able to run the engine and its client tools (for example `pg_dump`/`psql` for PostgreSQL).

With mirrord operator `3.216.0` or later, the branch pod pulls the image with the target's `imagePullSecrets`, as well as any set in the operator's branch config.

## Restricting Branch Images

Cluster admins can restrict which images are accepted, per database engine, with the `allowedImages` list in the operator's Helm values. Each entry is a glob pattern where `*` matches any substring:

```yaml
operator:
  pgBranchConfig:
    dbPod:
      allowedImages:
        - "registry.example.com/postgresql:*"
        - "docker.io/library/postgres:*"
```

A branch whose `image` matches no pattern is rejected and the session fails with an error. When `allowedImages` is **absent**, all images are allowed - restricting is an explicit, opt-in choice per cluster and engine. Each engine has its own `<engine>BranchConfig` block (`pgBranchConfig`, `mysqlBranchConfig`, `genericBranchConfig`, and so on); the list only affects branches that supply a custom `image`, so branches that rely on the default image are always allowed.

## Branch Storage

By default, each branch stores its database on its own PersistentVolumeClaims: one for the data directory and one for staging the dump during the copy, 20Gi each. The claims are provisioned with the cluster's default StorageClass when the branch is created and deleted together with it, so branching a large database does not depend on how much spare disk the node happens to have.

On clusters without a default StorageClass, branches automatically fall back to node-local `emptyDir` volumes, capped at 1Gi for data and 100Mi for the dump.

### Upgrading from older versions

{% hint style="warning" %}
Since operator `3.194.0`, per-branch PVCs are the default; older versions always ran branches on node-local `emptyDir` volumes. Upgrading needs **no config change**, and every explicit setting keeps its meaning. On clusters **without a default StorageClass**, branches keep running on `emptyDir` exactly as before (the operator logs a warning). Note that PVCs are provisioned storage: each branch now requests 20Gi for data and 20Gi for dump staging by default, billed by your cloud until the branch is deleted - tune the sizes below if that is too much.
{% endhint %}

| | Before | After |
| --- | --- | --- |
| Data volume | Node disk (`emptyDir`), 1Gi cap | Own PVC per branch; `databasePodVolumeLimit` if set, else 20Gi |
| Dump staging | Node disk (`emptyDir`), 100Mi cap | Own PVC per branch; `initPodVolumeLimit` if set, else 20Gi |
| Default memory limit | 512Mi | 2Gi |
| Cluster without a default StorageClass | n/a | Same `emptyDir` behavior as before, warning in the operator log |
| Explicit `dbPod.volume` / `initVolume` | Used as given | Unchanged - still overrides everything |

Sizing a branch for a large database used to mean raising the `emptyDir` caps and hoping the branch lands on a node with that much spare disk:

```yaml
# Before: caps on node-local scratch space, shared with everything on the node.
operator:
  dbBranching:
    initPodVolumeLimit: "1Gi"
    databasePodVolumeLimit: "50Gi"
```

Now the branch gets its own disk of the requested size, on any node:

```yaml
# After: provisioned per branch, deleted with it.
operator:
  dbBranching:
    databasePvcSize: "50Gi"
    initPvcSize: "50Gi"
```

The old `initPodVolumeLimit` / `databasePodVolumeLimit` values keep working after the upgrade: on the PVC path they size the claims (so a cluster tuned for 50Gi databases gets 50Gi PVCs, not the 20Gi default), and on `emptyDir` - the `kind: "emptyDir"` opt-out below, or the automatic fallback - they stay the size caps they always were. The new `databasePvcSize` / `initPvcSize` win when both are set.

### Tuning storage

Cluster admins can tune the storage per engine in the operator's Helm values:

```yaml
operator:
  dbBranching:
    # Cluster-wide default PVC sizes, per branch.
    databasePvcSize: "50Gi"
    initPvcSize: "50Gi"
  pgBranchConfig:
    dbPod:
      storage:
        # "pvc" (default) or "emptyDir".
        kind: "pvc"
        # Unset means the cluster's default StorageClass.
        storageClassName: "fast-ssd"
        # Per-engine overrides of the sizes above.
        dataSize: "100Gi"
        initSize: "100Gi"
```

To keep an engine's branches on node-local storage instead, opt out with `emptyDir`; the volumes are then capped by the `operator.dbBranching` limits:

```yaml
operator:
  dbBranching:
    # emptyDir caps: only used when branches run on emptyDir.
    initPodVolumeLimit: "100Mi"
    databasePodVolumeLimit: "1Gi"
  pgBranchConfig:
    dbPod:
      storage:
        kind: "emptyDir"
```

Setting `storageClassName` to a class that does not exist fails the branch with an error naming the fix, rather than leaving it stuck. An explicit `dbPod.volume` overrides the `storage` block entirely. The dump staged on the init volume is roughly the size of the copied data, so size both volumes for the database you branch.

Copying a large database also needs memory: the branch container's default memory limit is 2Gi, and admins can raise it per engine with `<engine>BranchConfig.dbPod.resources`.

## Branch Config Profiles

A profile is a named preset of branch pod settings that a cluster admin defines for an engine. A branch picks one with `profile` and runs with those settings.

Profiles are useful for two reasons:

* **Less to write in `mirrord.json`.** Settings that developers would otherwise repeat in every config - a custom image, a registry, pull secrets, resource limits - live in the profile instead. The branch names the preset and nothing else.
* **More than one baseline per cluster.** `<engine>BranchConfig.dbPod` is a single cluster-wide baseline: one registry, one set of pull secrets, one TLS mode. With profiles an engine can have several, so teams that need different branch pods can share a cluster - for example one team's Redis branches run a custom TLS image while another team's run a plain image from a different registry.

The admin defines profiles next to the default `dbPod`. A profile takes the same fields as `dbPod`, so an existing block can be moved into one unchanged:

```yaml
operator:
  redisBranchConfig:
    dbPod:
      allowedImages:
        - "registry.example.com/plain/redis:*"
    profiles:
      tls:
        dbPod:
          image:
            registry: "registry.example.com/tls/redis"
          imagePullSecrets:
            - name: "tls-registry-secret"
          allowedImages:
            - "registry.example.com/tls/redis:*"
          tls: true
          dbServerArgs:
            - "--tls-port"
            - "6379"
            - "--port"
            - "0"
            - "--tls-cert-file"
            - "/certs/tls.crt"
            - "--tls-key-file"
            - "/certs/tls.key"
```

A branch selects it by name, and needs no image, registry, or pull secret of its own:

```json
{
  "feature": {
    "db_branches": [
      {
        "type": "redis",
        "profile": "tls",
        "connection": {
          "url": "REDIS_URL"
        }
      }
    ]
  }
}
```

Branches that set no `profile` run with the default `dbPod`, so existing configs are unaffected. Naming a profile the operator does not define fails the session with an error listing the profiles that exist, rather than falling back to the default.

A profile is a complete `dbPod`, not a patch on the default one: fields it leaves unset fall back to the operator's built-in defaults, not to the values in the default `dbPod`. So a profile that sets only `resources` runs the operator's default image, not the registry configured in the default `dbPod`.

Besides pod settings, a profile can carry source-side defaults for the copy connection: `sourceSslmode` and `sourceTlsPropertyList` for engines with source TLS, and `sourceConfigMap` for apps whose connection details live in a ConfigMap-mounted config file (see [ConfigMap Source](db-branching/connection.md#configmap-source)).

Every profile setting lives under the profile's `dbPod` key, exactly as in the example above. A key placed beside `dbPod` instead - `profiles.tls.tls` rather than `profiles.tls.dbPod.tls` - is not a valid profile setting, and a branch selecting that profile fails with an error naming the misplaced key and where it belongs. Unrecognized keys outside profiles are ignored with a warning in the operator logs.

When a profile pins `image.registry`, the tag still comes from the branch's `version` or the engine's default tag. If your registry does not publish that default tag, developers set `version` and nothing else.

### Profiles and `allowedImages`

`allowedImages` is the one field a profile inherits, because leaving it unset means "any image is allowed" - a profile added for an unrelated reason should not quietly widen which images branches may run:

| `allowedImages` in the profile | Images the branch may run |
| --- | --- |
| not set | whatever the default `dbPod.allowedImages` allows, inherited unchanged |
| a list of globs | exactly those patterns - the default list does not apply |
| `["*"]` | any image |
| `[]` | none; the branch can only run the image the profile or operator provides |

The list only gates images a developer supplies with `image`. Branches that take their image from the profile or the operator's default registry are always allowed, so a profile with `allowedImages: []` still works for everyone who does not pin their own image.

Profiles work for every engine, each in its own `<engine>BranchConfig` block.

{% hint style="info" %}
Branch config profiles require operator and Helm chart `3.190.0` or later, and mirrord CLI `3.244.1` or later. Against an older operator, a branch that sets `profile` fails with a clear error instead of silently running the default settings.
{% endhint %}

## Branches Defined on the Workload

A cluster admin can define a workload's branches once, on its `MirrordSplitConfig`, and let every session take them from there with `"db_branches": "*"`. This is useful when several services share a database and their branches should match, or when `mirrord exec` and `mirrord up` must not drift: the definition lives next to the workload, not in each developer's `mirrord.json`.

The entries go under `dbBranches` on the workload's `MirrordSplitConfig` (the same resource that defines its queues for [queue splitting](queue-splitting.md); it needs no `queues`). Each entry is a `db_branches` entry with camelCase keys, an `id` sessions refer to, and a `copy` block that also lists the `allowedModes` a session may pick:

An entry differs from an inline `db_branches` entry in three ways, because it lives on the cluster rather than on a developer's machine:

* `location` is not accepted: a split config only branches databases that run in the cluster, never a local Redis.
* `migrations` need `name` (the source database they run against) and cannot use `path`, a local directory the operator cannot read; bake the files into an image and use `image` with `locations` (flyway) or `searchPath` (liquibase), or the container flavor.
* `generic` entries have no `copy.mode` or `copy.allowedModes`; a generic branch either runs a copy Job or starts empty.

```yaml
apiVersion: queues.mirrord.metalbear.co/v1
kind: MirrordSplitConfig
metadata:
  name: cake-maker
  namespace: bakery
spec:
  targetRef:
    apiVersion: apps/v1
    kind: Deployment
    name: cake-maker
  dbBranches:
    - id: orders-pg
      type: pg
      version: "16"
      ttlSecs: 1800
      connection:
        url: DATABASE_URL
      copy:
        mode: schema
        allowedModes: [schema, empty]
```

The operator checks every entry when the resource is applied and reports the verdict on its `Accepted` condition, so a typo shows up in `kubectl describe` instead of at the first session.

A session asks for every entry with `"*"`, or for some of them by id:

```json
{
  "feature": {
    "db_branches": "*"
  }
}
```

```json
{
  "feature": {
    "db_branches": ["orders-pg", "sessions-redis"]
  }
}
```

* An id that matches no entry fails the session, naming the ids the workload has. A workload with no `dbBranches` gives no branches, and the session continues without them.
* Inline entries in `mirrord.json` win entirely: with them, the session ignores the workload's `dbBranches` and says which `MirrordSplitConfig` it passed over.
* The branch id is the entry id plus the session key (`orders-pg-a1b2c3`), so every service started under one key that points at the same entry shares one branch. The first session creates it from its own entry; later ones attach, and only their copy mode is checked: a service whose entry asks for a different mode than the branch was created with fails, naming the service that created it. Two sessions that ask for the same branch at once end up on one branch.
* Each branch records the source it was copied from (host, port, database) in its status, as far as the operator can see it without reading Secrets. When two branches under one key were copied from the same database, the session that got the second one is told; if they were meant to be one branch, give both entries the same id.
* The feature needs a mirrord Operator that resolves `dbBranches`; against an older one the session fails before it starts, naming the operator version.

## Running With DB Branches

1. Run your app with mirrord and set the `db_branches` field in [the mirrord configuration file](https://metalbear.com/mirrord/docs/config).

2. mirrord will spin up a database branch according to the following rules:
 - Reusing an existing branch:
    If you provide an ID that matches an existing branch and its TTL hasn’t expired, mirrord will reuse that branch.
    When this happens, mirrord will notify you:
    ```
    A branch with this ID already exists for the target database.
    You’re about to use it! 
    ```
    *This means you’re connecting to an existing branch, not a fresh isolated one.*
 - Creating a new branch:
    If no ID is specified, or if you choose a new, non-existing ID, mirrord will create a fresh, empty database branch.

3. mirrord will override your DB environment variable with the branch's connection URL, so the app connects to the branch, not to the source db.
    This setup reduces the risk of accidental writes reaching the source database by directing activity toward an isolated branch.

4. The branch will be destroyed automatically when the TTL is reached and the branch is not in use (reconnecting to the same branch again extends its lifetime).

---

## Portforwards
When DB branching is enabled, mirrord will also automatically set up portforwards to the branch pod while the session is active. This can be used to, for example, access the branch database with a GUI SQL client like DBeaver or DataGrip. To list currently active DB branch portforwards, run `mirrord db-branches connections`. Branches provisioned in your cloud account, such as [S3](db-branching/s3.md), have no pod, so no portforward is set up for them.

---

## FAQ

**Q: Why does my connection time out?**
A: By default, branch databases have SSL disabled. Check if your client is specifically requesting SSL.

**Q: How do I use IAM authentication instead of passwords?**
A: mirrord supports IAM authentication for AWS RDS and GCP Cloud SQL, using the standard credential env vars already present on your target pod. See [IAM Authentication](db-branching/iam-authentication.md) for setup and examples.

## What's next?

Next, pick your database from [Choose Your Database](#choose-your-database) for engine-specific copy modes and configuration, check out [Connection Modes](db-branching/connection.md) for all the ways mirrord can locate your connection details, and see [Branch Management](db-branching/management.md) for CLI commands to inspect and destroy branches.
