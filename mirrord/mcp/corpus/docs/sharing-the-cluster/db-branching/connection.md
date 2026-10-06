---
title: Connection Modes
description: How mirrord locates your source database connection details for DB branching
tags:
  - team
  - enterprise
---

This page covers the `connection` field of a DB branch config - how mirrord locates the source database connection details. It applies to all database engines. For general concepts and the full list of config fields, see the [DB Branching overview](../db-branching.md).

mirrord supports two ways of specifying how to connect to the source database: a full **connection URL** or **individual connection parameters**. Either way, each value can come from the target pod's environment or from one of several other sources.

## Choose Your Connection Mode

Pick the shape that matches how your application gets its connection details:

* [Connection URL](#connection-url) - one variable holds the whole connection string.
* [Individual Connection Parameters](#individual-connection-parameters-params) - host, port, user, password, and database are separate values.
* [Value Sources](#value-sources) - where a value comes from when it is not a plain env var: a [Secret](#secret-source), a [ConfigMap](#configmap-source), [Google Secret Manager](#google-secret-manager-source), [AWS Secrets Manager](#aws-secrets-manager-source), or a [literal value](#literal-value).
* [Composite Environment Variables](#composite-environment-variables) - several details packed into one variable, picked apart with a regex.
* [Multiple Sources for the Same Parameter](#multiple-sources-for-the-same-parameter) - read/write splits and other duplicated variables.

## Connection URL

Provide a single environment variable that contains the full database connection string:

```json
{
  "connection": {
    "url": "DATABASE_URL"
  }
}
```

The optional `type` field controls where the environment variable is read from (applies to both URL and params modes). It defaults to `"env"` when omitted.

- `"env"` (default): Direct `env` entry in the target pod spec.
- `"env_from"`: From the target pod's `envFrom` field (`secretRef` or `configMapRef`). mirrord replicates the `envFrom` sources onto the init container so it can resolve the variable at runtime.

## Individual Connection Parameters (Params)

Instead of a single connection URL, you can specify each connection parameter separately. This is useful when your application stores host, port, user, password, and database as individual environment variables.

Available parameters: `url`, `host`, `port`, `user`, `password`, `database`. Each field is individually optional - mirrord fills in database-specific defaults for any parameters not specified. Non-existent environment variables are also filled with defaults. Specify the parameters that your application uses to connect to the database.

```json
{
  "connection": {
    "params": {
      "host": "DB_HOST",
      "port": "DB_PORT",
      "user": "DB_USER",
      "password": "DB_PASSWORD",
      "database": "DB_NAME"
    }
  }
}
```

Defaults
| Database | Port | User |
| --- | --- | --- |
| PostgreSQL | `5432` | `postgres` |
| MySQL | `3306` | `root` |
| MariaDB | `3306` | `root` |
| MSSQL | `1433` | `sa` |
| MongoDB | `27017` | `root` |
| Redis | `6379` | `default` |
| ClickHouse | `9000` | `default` |
| CockroachDB | `26257` | `root` |

Default for `connection.params.host` is `localhost` for all databases.

### URL as a Base

A `url` parameter holds a complete connection URL that every other parameter layers onto. Each component the URL carries - host, port, user, password, database - becomes that parameter's value, and a parameter declared alongside it overrides that component.

This suits an application that keeps one connection string instead of separate settings, especially when that string lives in a mounted config file while the credentials come from environment variables:

```json
{
  "connection": {
    "params": {
      "url": {
        "configmap": { "volume": "app-config" },
        "key": "application.yaml",
        "value_selector": ".datasource.url",
        "env_var_name": "APP_DATASOURCE_URL"
      },
      "user": "APP_DB_USER",
      "password": "APP_DB_PASSWORD"
    }
  }
}
```

The `url` parameter accepts every [value source](#value-sources) the other parameters do.

`env_var_name` matters more here than on the other parameters: it names the environment variable your local process receives the branch URL in. An application that reads one connection string has no separate host or port setting to redirect, so without it mirrord creates the branch and your application carries on talking to the source database.

The branch URL your application receives keeps the shape of the URL it was read from. The scheme, a `jdbc:` prefix, and any query parameters all survive - only the address and the database name are replaced. So an application whose URL reads `jdbc:mysql://prod-host:3306/app?useSSL=false&zeroDateTimeBehavior=convertToNull` gets `jdbc:mysql://<branch>:3306/<branch db>?useSSL=false&zeroDateTimeBehavior=convertToNull`, and driver options that change how results are read stay intact.

Where the branch needs a query parameter of its own, it replaces the application's value for that one parameter and leaves the rest alone.

The `url` parameter requires operator and Helm chart `3.212.0` or later, and mirrord CLI `3.264.0` or later. Against an older operator a branch that uses one fails up front with a clear error rather than waiting for a branch that is never created; an older CLI rejects the config as unknown.


### Custom Parameters

Besides the fixed slots, `params` accepts custom keys for engines that need them: [Google Spanner](spanner.md) declares its `project`/`instance`/`database_id` locators this way, PostgreSQL and [CockroachDB](cockroachdb.md#source-tls-and-mutual-tls) accept `sslmode` for the copy connection to the source, and [generic branches](generic.md) accept **any** key (for example `token`, `org`, `vhost`) - each is injected into the branch container as a `MIRRORD_PARAM_<NAME>` env var. Custom parameters support the same value sources as the fixed slots, and a literal `value` in one is extracted into the credential Secret exactly like the fixed slots. One difference from the fixed slots: a `value_pattern` on a custom parameter must name its capture group `value` (or use a plain first group) - per-name groups like `(?P<sslmode>...)` only work for the fixed slots.

### Branch Query Parameters (PostgreSQL)

The connection your application receives points at the branch pod, and its query parameters describe the branch rather than the source. `sslmode` is set automatically: `disable` for a regular branch pod, `require` when the operator's branch config enables TLS. So a source that mandates `?sslmode=require` (for example GCP Cloud SQL) works without changes - the branch connection drops the requirement the branch pod cannot serve.

To override the automatic values or add other parameters, set `query_params` on the branch config. This is useful when the branch runs a custom image with its own TLS setup, or when your application needs extra driver parameters on the branch connection:

```json
{
  "type": "pg",
  "connection": {
    "url": "DATABASE_URL"
  },
  "query_params": {
    "sslmode": "disable"
  }
}
```

Cluster admins can set the same overrides for everyone through the operator's branch config (`pgBranchConfig.dbPod.queryParams` in the Helm values, or on a [branch config profile](../db-branching.md#branch-config-profiles)). The layers merge per key: mirrord derives the default, the admin `queryParams` override it, and a session's own `query_params` from the mirrord config wins over both.

```yaml
pgBranchConfig:
  profiles:
    cloud-sql:
      dbPod:
        queryParams:
          sslmode: "disable"
```

`query_params` only affects the branch connection; the copy connection to the source keeps the source's own parameters.

Branch query params require operator and Helm chart `3.197.0` or later, and mirrord CLI `3.250.0` or later. Against an older operator, a branch that sets `query_params` (or an `sslmode` connection param) fails with a clear error instead of silently ignoring it.

## Value Sources

By default each connection value is the name of an env var on the target pod. Any value can instead come from one of the sources below; they can be mixed freely within one `params` block.

| Source | The value comes from | URL mode | Params mode |
| --- | --- | --- | --- |
| env var (default) | the target pod's `env` or `envFrom` | yes | yes |
| [`secret`](#secret-source) | a Kubernetes Secret, referenced by the branch pod | no | yes |
| [`configmap`](#configmap-source) | a ConfigMap entry, optionally a field inside a mounted JSON/YAML file | no | yes |
| [`gcp_secret_manager`](#google-secret-manager-source) | Google Secret Manager, fetched by the branch init container | yes | yes |
| [`aws_secrets_manager`](#aws-secrets-manager-source) | AWS Secrets Manager, fetched by the branch init container | yes | yes |
| [literal `value`](#literal-value) | the mirrord config itself | no | yes |

Every source accepts `env_var_name`: when set, mirrord hands the branch's value to your local process under that name.

### Secret Source

Any individual connection parameter can be sourced directly from a Kubernetes Secret instead of an environment variable. This is useful when credentials are stored in Kubernetes Secrets, such as AWS Secrets Manager synced secrets or volume-mounted secret files.

Instead of a plain string (env var name), use an object with `secret`, `key`, and `env_var_name`. The operator reads the Secret and injects the value under `env_var_name` for your local process, so your code can read it with `os.Getenv(...)` (or equivalent) regardless of whether the target pod exposes it:

```json
{
  "connection": {
    "params": {
      "host": "DB_HOST",
      "password": {
        "secret": "rds-credentials",
        "key": "password",
        "env_var_name": "DB_PASSWORD"
      },
      "database": "DB_NAME"
    }
  }
}
```

In this example, `host` and `database` are read from environment variables, while `password` is read directly from the `rds-credentials` Kubernetes Secret (key `password`).

{% hint style="info" %}
The `secret` source is only supported for individual connection parameters, not for the full connection URL.
{% endhint %}

### ConfigMap Source

Any individual connection parameter can also be read from a Kubernetes ConfigMap. This is useful when your application takes its database settings from a config file mounted from a ConfigMap rather than from environment variables.

Instead of a plain string, use an object with `configmap`, `key`, and optionally `value_selector` (or `value_pattern`) and `env_var_name`:

```json
{
  "connection": {
    "params": {
      "host": {
        "configmap": { "volume": "app-config" },
        "key": "config.yml",
        "value_selector": ".database.host",
        "env_var_name": "DB_HOST"
      },
      "port": {
        "configmap": { "volume": "app-config" },
        "key": "config.yml",
        "value_selector": ".database.port",
        "env_var_name": "DB_PORT"
      },
      "user": "DB_USER",
      "password": "DB_PASSWORD"
    }
  }
}
```

The fields:

* `configmap` - which ConfigMap to read. Either its name (`"configmap": "app-config"`), or a `configMap` volume of the target pod (`"configmap": { "volume": "app-config" }`). Prefer the volume form when your deployment tool renames the ConfigMap on every release (for example a version suffix added by ArgoCD or Helm): the volume name in the pod spec stays the same while the ConfigMap it points at changes.
* `key` - the entry in the ConfigMap's `data`. With the volume form, this is the file name inside the volume, so a volume that remaps keys via `items` is resolved through that mapping.
* `value_selector` - a selector run over the entry parsed as JSON or YAML. It supports nested keys (`.database.host`) and `.[]` to iterate arrays or object values; pipes, functions, and other jq operators are not supported. The selector must land on exactly one string, number, or boolean.
* `value_pattern` - a regex whose capture group marks the value inside the raw entry text, for entries that are not JSON or YAML. It follows the same capture group rules as [composite environment variables](#composite-environment-variables). Mutually exclusive with `value_selector`.
* `env_var_name` - optional. When set, mirrord injects the branch connection under that name for your local process, just like the `secret` source, so your code can read it with `os.Getenv(...)` (or equivalent). Without it, the value is only used to build the branch.

Without `value_selector` or `value_pattern`, the whole entry (trimmed) is the value.

### Setting the ConfigMap Once in a Profile

When every developer's config would repeat the same `configmap` and `key`, the cluster admin can set them once with `dbPod.sourceConfigMap` in the operator's branch config, either on the default `dbPod` or on a [branch config profile](../db-branching.md#branch-config-profiles):

```yaml
mysqlBranchConfig:
  profiles:
    app-config:
      dbPod:
        sourceConfigMap:
          volume: app-config     # or `name: <ConfigMap name>`
          key: config.yml        # optional; params can name their own key
```

A param then only carries its selector and the local variable name, and picks the profile:

```json
{
  "type": "mysql",
  "profile": "app-config",
  "connection": {
    "params": {
      "host": { "value_selector": ".database.host", "env_var_name": "MYSQL_HOST" },
      "port": { "value_selector": ".database.port", "env_var_name": "MYSQL_PORT" },
      "database": { "value_selector": ".database.name", "env_var_name": "MYSQL_DB" },
      "user": "MYSQL_USERNAME",
      "password": "MYSQL_PASSWORD"
    }
  }
}
```

The layering is per field: a param's own `configmap` or `key` wins over the profile's, and the profile fills in whatever the param leaves out. A param that omits `configmap` on a profile without `sourceConfigMap` (or omits `key` on both sides) fails the branch with an error naming both places to fix it.

One rule to remember: a param with only `value_pattern` and `env_var_name` is an [environment variable pattern](#composite-environment-variables), not a ConfigMap source. To use `value_pattern` against the profile's ConfigMap, keep `key` (or `configmap`) on the param so it stays a ConfigMap source `value_selector` has no such overlap.

The operator reads the ConfigMap itself when the branch is created, so it needs `get` on ConfigMaps in the target namespace; the operator Helm chart grants this together with the other DB branching permissions.

ConfigMap sources require operator and Helm chart `3.205.0` or later, and mirrord CLI `3.255.0` or later. Against an older operator, a branch that uses one fails up front with a clear error instead of waiting for a branch the operator never creates; an older CLI rejects the config as unknown.

{% hint style="info" %}
Your local application still has to pick the branch up. With `env_var_name`, the branch host is delivered as an environment variable, which works when your app lets an environment variable override the value from its config file. If your app only ever reads the mounted file, the file itself is not rewritten.
{% endhint %}

### Google Secret Manager Source

Any connection value can be read from [Google Secret Manager](https://cloud.google.com/secret-manager) instead of an environment variable or a Kubernetes Secret. This is useful when your application already loads its database credentials from Secret Manager at runtime and never puts them in the pod spec.

The branch init container fetches the value when it copies the data, using the target pod's service account through [GKE Workload Identity](https://cloud.google.com/kubernetes-engine/docs/concepts/workload-identity) - the same way [GCP Cloud SQL IAM](iam-authentication.md#gcp-cloud-sql-iam-authentication) works. mirrord and the operator never read the secret themselves.

Unlike the `secret` source, this works for both the full connection URL and individual parameters.

For the full URL, use `type: gcp_secret_manager` with a `secret_ref` (the Secret Manager resource name):

```json
{
  "connection": {
    "url": {
      "type": "gcp_secret_manager",
      "secret_ref": "projects/my-project/secrets/db-url/versions/latest",
      "env_var_name": "DATABASE_URL"
    }
  }
}
```

For an individual parameter, use a `gcp_secret_manager` field with the resource name:

```json
{
  "connection": {
    "params": {
      "host": "DB_HOST",
      "password": {
        "gcp_secret_manager": "projects/my-project/secrets/db-password/versions/latest",
        "env_var_name": "DB_PASSWORD"
      },
      "database": "DB_NAME"
    }
  }
}
```

`env_var_name` is optional. When set, the operator injects the branch connection under that name for your local process, just like the `secret` and literal-value sources, so your code can read it with `os.Getenv(...)` (or equivalent). Without it, the value is only used to build the branch and your local process keeps reading its own source.

One exception: container-flavor [migrations](migrations.md) inherit the target's environment, and the operator must redirect the declared connection variables to the branch inside the migration Job. A `secret`, `gcp_secret_manager`, or `aws_secrets_manager` source without `env_var_name` gives it no variable name to redirect, so the migration fails with an error instead of running with the source connection in its environment. Set `env_var_name` to the variable your app reads, or have the cluster admin disable `migrationEnv.inherit` in the operator's branch config.

{% hint style="info" %}
**Setup**: the branch pod inherits the target pod's service account, so that account's Google identity must have `roles/secretmanager.secretAccessor` on the secret. No operator-level permissions are needed.
{% endhint %}

### AWS Secrets Manager Source

Any connection value can also be read from [AWS Secrets Manager](https://aws.amazon.com/secrets-manager/). This works the same way as the Google Secret Manager source: the branch init container fetches the value when it copies the data, using the target pod's service account through IRSA or EKS Pod Identity - the same way [AWS RDS IAM](iam-authentication.md#aws-rds-iam-authentication) works. mirrord and the operator never read the secret themselves.

The secret reference is a secret name or a full ARN. For the full URL, use `type: aws_secrets_manager` with a `secret_ref`:

```json
{
  "connection": {
    "url": {
      "type": "aws_secrets_manager",
      "secret_ref": "arn:aws:secretsmanager:us-east-1:123456789012:secret:db-url",
      "env_var_name": "DATABASE_URL"
    }
  }
}
```

For an individual parameter, use an `aws_secrets_manager` field:

```json
{
  "connection": {
    "params": {
      "host": "DB_HOST",
      "password": {
        "aws_secrets_manager": "db-password",
        "env_var_name": "DB_PASSWORD"
      },
      "database": "DB_NAME"
    }
  }
}
```

`env_var_name` and the migration exception work exactly as described for the Google Secret Manager source above.

{% hint style="info" %}
**Setup**: the branch pod inherits the target pod's service account, so that account's AWS identity must be allowed `secretsmanager:GetSecretValue` on the secret. The region comes from the ARN when a full ARN is given; for a plain secret name, `AWS_REGION` or `AWS_DEFAULT_REGION` must be set in the target pod spec.
{% endhint %}

### Literal Value

You can provide a connection parameter as a literal value directly in the config. This is useful when the credential is injected at runtime by an external system and does not appear in the pod spec where mirrord can read it.

Use a field with `value`:

```json
{
  "connection": {
    "params": {
      "host": "DB_HOST",
      "port": "DB_PORT",
      "user": "DB_USER",
      "password": { "env_var_name": "DB_PASSWORD", "value": "my-db-password" },
      "database": "DB_NAME"
    }
  }
}
```

Works for any connection parameter (`host`, `port`, `user`, `password`, `database`). The CLI stores the literal value in a Kubernetes Secret. The operator uses it to connect the branch DB to the source and also injects it under the name you set in `env_var_name` for your local process, so your code can read it with `os.Getenv(...)` (or equivalent) even when the target pod doesn't expose it.

## Composite Environment Variables

Some applications pack multiple connection details into a single environment variable. For example, a target pod might expose:

```yaml
- name: DB_SERVER
  value: "prod-db.internal:5432"
```

Here `host` and `port` live inside the same `DB_SERVER` value. Use `value_pattern` to specify which part of the value belongs to which parameter. The pattern works on both `params` fields and `url` sources.

```json
{
  "connection": {
    "params": {
      "host": {
        "env_var_name": "DB_SERVER",
        "value_pattern": "^(?P<host>[^:]+):\\d+$"
      },
      "port": {
        "env_var_name": "DB_SERVER",
        "value_pattern": "^[^:]+:(?P<port>\\d+)$"
      },
      "user": "DB_USER",
      "password": "DB_PASSWORD",
      "database": "DB_NAME"
    }
  }
}
```

During a session, only the matched part of the value is swapped out: just the host, or just the port. The rest of the string always stays intact, so your app still sees `DB_SERVER` in the `host:port` format it expects.

### Choosing the Capture Group

The capture group name follows the parameter name - `(?P<host>...)` for the `host` variable, `(?P<port>...)` for the `port` variable.

For single-parameter patterns you can also use `(?P<value>...)` as a generic name, or a plain unnamed group like ([^:]+). If the regex contains more than one unnamed group, the first one is used.

> The regex must contain at least one capture group, otherwise the configuration is rejected.

## Multiple Sources for the Same Parameter

Both `url` and individual `params` fields accept either a single value or an array. This is useful when an application uses several env vars for the same logical connection. For example, separate read/write URLs.

> **Rule:** the **first entry** in the array is used to locate the source database and clone it. During the session, **every entry** is rewritten to point at the branch pod.
> 

```json
{
  "connection": {
    "url": ["DATABASE_WRITE_URL", "DATABASE_READ_URL"]
  }
}
```

The **first entry** is used to locate the source database and clone it. During the session, **every entry** is rewritten to point at the branch pod. In the example above, `DATABASE_WRITE_URL` is read to find the source database, but both `DATABASE_WRITE_URL` and `DATABASE_READ_URL` are redirected to the branch, so the application reads and writes against the same branch instead of pointing reads at the original database.

### Combining Arrays with `value_pattern`

If the same connection parameter appears in multiple env vars and each var encodes a composite value, use an array of `value_pattern` objects. 

As with plain arrays, the first entry is used as the source. Even if `WRITE_SERVER` and `READ_SERVER` point to different databases, only `WRITE_SERVER` is cloned. During the session, all entries are rewritten to point at the branch.

For example, when both `WRITE_SERVER` and `READ_SERVER ` hold a `host:port` pair:

```json
{
  "connection": {
    "params": {
      "host": [
        { "env_var_name": "WRITE_SERVER", "value_pattern": "^([^:]+):" },
        { "env_var_name": "READ_SERVER", "value_pattern": "^([^:]+):" }
      ],
      "port": [
        { "env_var_name": "WRITE_SERVER", "value_pattern": ":(\\d+)$" },
        { "env_var_name": "READ_SERVER", "value_pattern": ":(\\d+)$" }
      ],
      "user": ["DB_USER", "DB_READ_USER"],
      "password": ["DB_PASSWORD", "DB_READ_PASSWORD"],
      "database": "DB_NAME"
    }
  }
}
```

The same rule applies: `WRITE_SERVER` (the first entry) is used to extract the source connection. During the session, all entries - `WRITE_SERVER`, `READ_SERVER`, both user vars, and both password vars - are rewritten to point at the branch.
