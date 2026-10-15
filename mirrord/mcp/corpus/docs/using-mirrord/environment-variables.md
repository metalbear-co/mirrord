---
title: Environment Variables
description: Control which environment variables are loaded from the remote pod
draft: false
toc: true
tags: ["oss", "team", "enterprise"]
---

By default, mirrord imports all environment variables from the remote pod into your local process. This means your local code automatically gets the same database URLs, API keys, feature flags, and service discovery values as the deployed application, without any manual setup.

Local environment variables that aren't present in the remote pod are preserved. When a variable exists in both, the remote value wins.

## Including only specific variables

If you only need a few remote variables (e.g. a database connection string), use `include` to allowlist them:

```json
{
  "feature": {
    "env": {
      "include": "DATABASE_URL;API_KEY;REDIS_HOST"
    }
  }
}
```

Variables are separated by semicolons. Supports regex patterns: `"include": "DB_.*"`.

## Excluding specific variables

If most remote variables are useful but a few cause problems locally, use `exclude`:

```json
{
  "feature": {
    "env": {
      "exclude": "PATH;HOME;USER;SHELL"
    }
  }
}
```

`include` and `exclude` are mutually exclusive. Use one or the other.

## Overriding values

Sometimes you want the remote variable but with a different value. For example, pointing at a local database while keeping everything else remote:

```json
{
  "feature": {
    "env": {
      "override": {
        "DATABASE_URL": "postgres://localhost:5432/mydb",
        "DEBUG": "true"
      }
    }
  }
}
```

Overrides are applied after remote variables are loaded, so they take priority over both remote and local values.

## Loading from a file

You can load overrides from a dotenv-style file:

```json
{
  "feature": {
    "env": {
      "env_file": ".env.local"
    }
  }
}
```

## Mapping variable names

If your local code expects a different variable name than what the remote pod uses:

```json
{
  "feature": {
    "env": {
      "mapping": {
        "REMOTE_DB_URL": "DATABASE_URL"
      }
    }
  }
}
```

This maps the remote `REMOTE_DB_URL` value into the local `DATABASE_URL` variable.

## Unsetting variables

To ensure a remote variable is *not* present in your local process at all:

```json
{
  "feature": {
    "env": {
      "unset": "KUBERNETES_SERVICE_HOST;KUBERNETES_PORT"
    }
  }
}
```

## Common scenarios

**"My app connects to the wrong database locally"** The remote `DATABASE_URL` is being imported. Either override it with a local value, or exclude it.

**"I need remote env vars but PATH keeps getting overwritten"** Exclude `PATH` (and other shell variables like `HOME`, `USER`, `SHELL`).

**"I want to test with a feature flag enabled"** Use `override` to set the flag value, regardless of what the remote pod has.

For the full list of environment variable settings, see the [configuration reference](https://metalbear.com/mirrord/docs/config#feature.env).
For a technical explanation of how environment variables work under the hood, see the [Environment Variables reference](../reference/env.md).
