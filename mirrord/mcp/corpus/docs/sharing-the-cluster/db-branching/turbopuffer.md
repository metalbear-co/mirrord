---
title: turbopuffer
description: Spin up an isolated turbopuffer namespace branch of your remote namespace with mirrord
tags:
  - alpha
  - team
  - enterprise
---

This page covers DB branching for [turbopuffer](https://turbopuffer.com) namespaces. For the general concepts, the full list of config fields, and how a session behaves, see the [DB Branching overview](../db-branching.md).

Unlike the engines mirrord runs as pods in your cluster, a turbopuffer branch is a namespace in your own turbopuffer account: the operator asks turbopuffer to [branch](https://turbopuffer.com/docs/write#param-branch_from_namespace) your source namespace into a copy-on-write clone, and points your application at the clone. Nothing runs in the cluster, so there is no `image`, `version`, `profile` or migrations.

{% hint style="info" %}
turbopuffer branching requires operator `3.212.0`, mirrord CLI `3.264.0`, and operator Helm chart `3.212.0` with the `operator.turbopufferBranching` value set to `true`.
{% endhint %}

## Basic Configuration

```json
{
  "feature": {
    "db_branches": [
      {
        "id": "docs-turbopuffer",
        "type": "turbopuffer",
        "source": {
          "params": {
            "namespace": "TPUF_NAMESPACE",
            "api_key": "TURBOPUFFER_API_KEY",
            "region": "TURBOPUFFER_REGION"
          }
        },
        "copy": {
          "mode": "all"
        }
      }
    ]
  }
}
```

### Source

turbopuffer clients pick the namespace on every request, so mirrord can only redirect your application if it reads the namespace name from an environment variable. `source.params` names where the operator finds what it needs on the target workload:

| Param | Required | Rewritten in the session | Meaning |
| --- | --- | --- | --- |
| `namespace` | yes | yes | The env var holding the source namespace name. The session sees the branch namespace in it instead. |
| `api_key` | yes | no | The API key the operator branches and later deletes the namespace with. |
| `region` | one of the two | no | The turbopuffer region the namespace lives in, for example `gcp-us-central1`. |
| `base_url` | one of the two | no | The full API endpoint instead of a region, for dedicated clusters. Its host must be on a domain the operator allows (see below). |

Each param accepts the same sources as any other engine's connection params (see [Connection Modes](connection.md)): a plain env var name, a Kubernetes Secret, a regex extracting the value out of a larger variable, or a literal value. A turbopuffer branch has no branch pod, so the operator resolves these itself: a param held in Google Secret Manager or AWS Secrets Manager must be readable by the operator, not just by the target's service account. A literal is handy when your application sets the region in code rather than in its environment:

```json
{
  "source": {
    "params": {
      "namespace": { "env_var_name": "TPUF_URI", "value_pattern": "namespaces/(?P<value>[^/]+)" },
      "api_key": { "secret": "turbopuffer", "key": "api-key" },
      "region": { "env_var_name": "TURBOPUFFER_REGION", "value": "gcp-us-central1" }
    }
  }
}
```

The `namespace` param must name an env var, since that is what the session rewrites; a Secret-backed `namespace` needs an `env_var_name`.

The endpoint a branch resolves to is where the operator sends the API key, so it is not the branch author's to choose freely: the operator refuses any host outside `turbopuffer.com`. A dedicated cluster served from another domain is added by whoever installs the operator, through the chart's `operator.turbopufferOptions.allowedHosts`:

```yaml
operator:
  turbopufferBranching: true
  turbopufferOptions:
    allowedHosts:
      - tpuf.internal.example.com
```

## Copy Modes

| Mode | What the branch starts with | Best for |
| --- | --- | --- |
| `"empty"` (default) | Nothing. The branch is a fresh namespace name that turbopuffer creates on your application's first write. | Applications that build their own index, or when the source is far too large to care about. |
| `"all"` | An instant copy-on-write clone of the source namespace: every document and the schema. | A full clone of your environment data for debugging or reproducing production-like scenarios. |

Branching is done entirely inside turbopuffer, so a full clone costs no copy time and no cluster bandwidth. turbopuffer bills each branch operation at a flat rate; see [their pricing](https://turbopuffer.com/pricing).

## Cleanup

The branch namespace is deleted when the branch expires or is destroyed with `mirrord db-branches destroy`. The operator keeps the API key it needs for that deletion in a Secret in its own namespace for as long as the branch namespace exists, so deleting the target's namespace does not strand the branch namespace.

## Known Limitations

{% hint style="warning" %}
- **Whole namespaces only.** turbopuffer branches a namespace as a unit; there is no filter to copy part of one.
- **Applications that compute namespace names at runtime** (for example one namespace per tenant) cannot be redirected, since only a single env var is rewritten.
{% endhint %}
