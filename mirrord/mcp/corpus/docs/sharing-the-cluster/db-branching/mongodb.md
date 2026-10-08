---
title: MongoDB
description: Spin up an isolated MongoDB branch of your remote database with mirrord
tags:
  - beta
  - team
  - enterprise
---

This page covers DB branching for MongoDB. For the general concepts, the full list of config fields, and how a session behaves, see the [DB Branching overview](../db-branching.md).

{% hint style="info" %}
MongoDB branching requires operator `3.137.0`, mirrord CLI `3.183.0`, and operator Helm chart `1.44.0` with the `operator.mongoBranching` value set to `true`.
{% endhint %}

## Basic Configuration

```json
{
  "feature": {
    "db_branches": [
      {
        "id": "users-mongo-db",
        "type": "mongodb",
        "version": "7.0",
        "name": "users-database-name",
        "connection": {
          "url": "MONGODB_URI"
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

MongoDB supports two copy modes. The copy mode sets the **default behavior** for all collections.
When combined with [collection filters](#collection-filters), the mode determines what happens to collections that are _not_ listed in the filter. Filtered collections always receive only the matching documents.

| Mode | What gets cloned | Best for |
| --- | --- | --- |
| `"empty"` (default) | Nothing - an empty database | Workflows where your application initializes the collections or runs migrations as part of startup |
| `"all"` | All collections and data from the source database | A full clone of your environment data for debugging or reproducing production-like scenarios |

{% hint style="warning" %}
Use `"mode": "all"` with caution.
It's only recommended for very small or empty databases.
Copying large datasets can significantly increase branch creation time and storage usage.
{% endhint %}

{% hint style="info" %}
MongoDB does not support a `"schema"` copy mode. In relational databases, `"schema"` copies table structures without data. MongoDB is schema-less and collections don't have a predefined structure separate from their documents, so `"empty"` and `"all"` are the available options.
{% endhint %}

### Collection Filters

Developers can customize which collections are copied and apply MongoDB query filters per collection.

The copy mode controls the **baseline** (what happens to collections not mentioned in your filters):

| Mode | Unfiltered collections | Filtered collections |
|------|----------------------|---------------------|
| `"empty"` | Not created | Created with matching documents only |
| `"all"` | Fully copied (all documents) | Copied with matching documents only |

#### Example: `"mode": "all"` with filters

```json
{
  "copy": {
    "mode": "all",
    "collections": {
      "users": {
        "filter": "{\"name\": {\"$in\": [\"alice\", \"bob\"]}}"
      },
      "orders": {
        "filter": "{\"created_at\": {\"$gt\": 1759948761}}"
      }
    }
  }
}
```

All collections are copied, but the `users` collection includes only documents for alice and bob, and the `orders` collection includes only documents created after the given timestamp.

#### Example: `"mode": "empty"` with filters

```json
{
  "copy": {
    "mode": "empty",
    "collections": {
      "users": {
        "filter": "{\"name\": {\"$in\": [\"alice\", \"bob\"]}}"
      }
    }
  }
}
```

Only the `users` collection is created, containing documents for alice and bob. All other collections are not created. This is useful when you only need a subset of reference data and your application handles the rest through migrations.

{% hint style="info" %}
The `dump_args` field is not supported for MongoDB. MongoDB branches use their own internal dump mechanism, so only MySQL and PostgreSQL branches accept custom dump arguments.
{% endhint %}
