---
title: Preview Environments in CI
description: How to automate preview environments in your CI pipeline with PR-based keys and header propagation
date: 2026-02-23T00:00:00.000Z
lastmod: 2026-02-23T00:00:00.000Z
draft: false
toc: true
tags:
  - enterprise
  - beta
menu:
  docs:
    parent: preview-environments
---

This guide explains how to automate [Preview Environments](preview-environments.md) in your CI pipeline. By integrating mirrord preview with your pull request workflow, each PR gets its own ephemeral environment that reviewers can access via a shared URL and a simple header—no local mirrord setup required.

{% hint style="info" %}
This feature is available to users on the **Enterprise** pricing plan.
{% endhint %}

## Flow Overview

1. **On PR open or push:** Your CI builds the image(s), pushes to a registry, runs `mirrord preview start` with a stable key (e.g. `pr-123`), and posts or updates a comment on the PR with the preview URL and the header to use.
2. **Reviewers:** Open the shared URL and send the header (via the [mirrord Browser Extension](../using-mirrord/incoming-traffic/debug-from-browser.md) or `curl`). Traffic matching the header is routed to the preview pod.
3. **On PR merge or close:** CI runs `mirrord preview stop -k <key>` to tear down the preview environment.

## Cluster Access for CI

Your CI job needs a kubeconfig to run `mirrord preview` commands, but it doesn't need (and shouldn't get) cluster-admin credentials. The mirrord operator Helm chart ships a `mirrord-operator-ci` ClusterRole scoped to exactly what CI needs: creating and deleting preview sessions, plus the operator APIs the CLI talks to.

Setting this up takes three steps: create an identity for CI, grant it the role, and package its credentials as a kubeconfig your CI can use.

### 1. Create a ServiceAccount, bind the role, mint a token

Kubernetes RBAC separates identity from permissions, so this needs three objects: the **ServiceAccount** is the identity CI authenticates as, the **ClusterRoleBinding** attaches the chart's `mirrord-operator-ci` ClusterRole to it, and the **Secret** asks Kubernetes to issue a long-lived token for it:

```yaml
# The identity CI authenticates as.
apiVersion: v1
kind: ServiceAccount
metadata:
  name: preview-ci
  namespace: staging
---
# Grants that identity the operator chart's CI role.
apiVersion: rbac.authorization.k8s.io/v1
kind: ClusterRoleBinding
metadata:
  name: preview-ci-mirrord
subjects:
  - kind: ServiceAccount
    name: preview-ci
    namespace: staging
roleRef:
  kind: ClusterRole
  name: mirrord-operator-ci
  apiGroup: rbac.authorization.k8s.io
---
# A long-lived token for the ServiceAccount, consumed in step 2.
apiVersion: v1
kind: Secret
metadata:
  name: preview-ci-token
  namespace: staging
  annotations:
    kubernetes.io/service-account.name: preview-ci
type: kubernetes.io/service-account-token
```

### 2. Build a kubeconfig from the token

A ServiceAccount doesn't come with a kubeconfig — assemble one from the API server address, the cluster CA, and the token issued in step 1:

```bash
SERVER=$(kubectl config view --minify -o jsonpath='{.clusters[0].cluster.server}')
CA=$(kubectl -n staging get secret preview-ci-token -o jsonpath='{.data.ca\.crt}')
TOKEN=$(kubectl -n staging get secret preview-ci-token -o jsonpath='{.data.token}' | base64 -d)

cat > preview-ci.kubeconfig <<EOF
apiVersion: v1
kind: Config
clusters:
- name: cluster
  cluster:
    server: ${SERVER}
    certificate-authority-data: ${CA}
users:
- name: preview-ci
  user:
    token: ${TOKEN}
contexts:
- name: preview-ci
  context:
    cluster: cluster
    user: preview-ci
current-context: preview-ci
EOF
```

### 3. Store it as a CI secret

Base64-encode the file (`base64 -w0 preview-ci.kubeconfig`) and save it as a CI secret, e.g. `KUBECONFIG_DATA` in GitHub Actions. In the workflow, decode it and point `KUBECONFIG` at it before running any `mirrord preview` commands:

```yaml
- name: Configure cluster access
  env:
    KUBECONFIG_DATA: ${{ secrets.KUBECONFIG_DATA }}
  run: |
    echo "$KUBECONFIG_DATA" | base64 -d > kubeconfig
    echo "KUBECONFIG=$PWD/kubeconfig" >> "$GITHUB_ENV"
```

A token with only this ClusterRole can manage preview environments but can't read or modify other cluster resources.

## Choosing the Preview Key

Use a deterministic key tied to the PR so that:

- The same key is used across pushes, allowing you to update the PR comment instead of creating duplicates.
- Cleanup is straightforward: when the PR closes, you stop the preview by that key.

**Best practice:** Use the PR number (e.g. `pr-123`) or a sanitized branch name. In GitHub Actions:

```yaml
env:
  PREVIEW_KEY: "pr-${{ github.event.pull_request.number }}"
```

## Automating the PR Comment

Post a comment on the PR that includes:

| Field | Example |
|-------|---------|
| **Preview URL** | `https://myapp.example.com` |
| **Header** | `baggage: mirrord-session=pr-123` |

Also include instructions for reviewers:

- Use the [mirrord Browser Extension](../using-mirrord/incoming-traffic/debug-from-browser.md) to set the header for the preview URL, or
- Use `curl -H "baggage: mirrord-session=pr-123" https://myapp.example.com/api/...`

**Best practice:** Find an existing comment (e.g. by a marker like `## mirrord Preview Environment`) and update it on each push, instead of creating a new comment every time. This keeps the PR tidy.

## Header Propagation for Backend Testing

When your frontend calls a backend, and the backend calls other services (databases, APIs, queues), the preview header must be propagated so downstream traffic is routed correctly. In many stacks, the best first step is to enable W3C context propagation in the observability or tracing library you already use, since most modern frameworks and OpenTelemetry-based integrations can forward `baggage` and `tracestate` automatically.

### 1. Configure the Header Filter in mirrord

In your `mirrord-preview.json`, set the HTTP filter to match on a header of your choice so only requests with the matching value hit the preview pod. The example below uses the W3C `baggage` header, but any header works:

```json
{
  "target": {
    "path": "deployment/my-backend",
    "namespace": "staging"
  },
  "feature": {
    "preview": {
      "ttl_mins": 120,                   // Or `ttl_secs` — pick whichever unit is more convenient
      "creation_timeout_secs": 600
    },
    "network": {
      "incoming": {
        "mode": "steal",
        "http_filter": {
          "header_filter": "^baggage: .*mirrord-session={{ key }}.*"
        }
      }
    }
  }
}
```

The `{{ key }}` template is replaced by mirrord at runtime with the environment key passed via `-k` (e.g. `pr-123`). This lets you commit one config file that works for every PR — each preview pod only steals requests carrying its own key. Don't hardcode a key in the filter: the config is shared across PRs, so a hardcoded value would route every reviewer to the same (wrong) preview pod.

### 2. Propagate the Header in Your Application

First, check whether your existing tracing or observability library can propagate W3C headers for you. If it can, prefer enabling that instead of adding custom propagation code.

If you (or your observability library) don't propagate the header, read it from the incoming request, store the preview key in request context, and forward it on all outgoing calls:

- **HTTP:** Add the header to outgoing `http.Request` objects.
- **gRPC:** Add it to `metadata` in the outgoing context.
- **Kafka:** Add it to message headers.
- **SQS:** Add it to message attributes.

Example (Go with Gin): read `baggage`, set it in context, then add it to outgoing HTTP and gRPC calls:

```go
baggage := c.GetHeader("baggage")
if baggage != "" {
    c.Set("baggage", baggage)
}
// Later, when making outgoing HTTP request:
req.Header.Set("baggage", baggage)
// Or for gRPC:
md := metadata.Pairs("baggage", baggage)
ctx := metadata.NewOutgoingContext(c, md)
```

If you (or your observability library) don't propagate the header, downstream services won't know which preview environment the request belongs to, and traffic may not reach the correct preview pods.

## Registry Authentication

The preview image must be pullable **from inside the cluster**. The preview pod is a copy of the target's pod spec with the image swapped, so it pulls with the same credentials as the target — there is no separate registry configuration for previews.

In practice this means: push preview image tags to the same registry and repository the target already pulls from (the image your target deployment runs, tagged per PR, e.g. `preview-pr-123-abc1234`). Pulls then work with zero extra setup. If CI pushes the preview image somewhere the target can't pull from — a different registry, or a brand-new package with no access configured — the preview fails with `ErrImagePull`.

## mirrord Configuration

Use the same config file as above (e.g. `mirrord-preview.json`) per service. The image can be provided in the config or overridden with `-i` in CI. In CI, pass the image and key via CLI:

```bash
mirrord preview start \
  -f mirrord-preview.json \
  -i "ghcr.io/org/my-app:preview-pr-123-abc1234" \
  -k "pr-123" \
  --force \
  --timeout 600
```

The `--force` flag makes repeat runs with the same key replace the existing preview pod — which is exactly what you want when a PR gets a new push (see below).

## CI Workflow Best Practices

- **Concurrency:** Use a concurrency group per PR so that new pushes cancel in-progress runs and avoid duplicate preview pods:
  ```yaml
  concurrency:
    group: preview-env-${{ github.event.pull_request.number }}
    cancel-in-progress: true
  ```

- **New pushes:** Pass `--force` to `mirrord preview start` so a push to an open PR replaces the existing preview pod with the new image. Without it, the CLI refuses to start a session when one already exists for the same key and target.

- **Image tags:** Include both PR number and commit SHA for traceability, e.g. `preview-pr-123-abc1234`.

- **TTL:** Set `ttl_mins` comfortably longer than your typical review session. The TTL is a leak guard, not the primary cleanup — the PR-close job is. Each push replaces the preview (see `--force` above), which also resets the TTL, but a preview on an untouched PR expires quietly once the TTL elapses; re-run the workflow to bring it back. If you want the preview to live exactly as long as the PR, set `"ttl_mins": "infinite"` and rely on the PR-close job for cleanup — just be aware there's no leak guard if that job ever fails to run.

- **Cleanup:** Always run `mirrord preview stop` when the PR is closed. Use `|| true` so the job doesn't fail if the preview was already stopped or never started:
  ```bash
  mirrord preview stop -k "pr-${{ github.event.pull_request.number }}" || true
  ```

- **Multiple services:** Use the same key for all preview pods in a PR so they form one logical environment. Run `mirrord preview start` once per service, each with `-k "pr-123"`.

## Troubleshooting

| Symptom | Cause and fix |
|---------|---------------|
| `ErrImagePull`, `failed to authorize` / `401 Unauthorized` | The cluster can't pull the preview image. Push preview tags to the same registry and repository the target already pulls from — see [Registry Authentication](#registry-authentication). One common trap: a brand-new `ghcr.io` package created by a workflow's `GITHUB_TOKEN` starts out private. |
| `preview start` refuses because a session already exists for the key and target | A previous run's session is still alive. Pass `--force` to replace it. |
| The preview fails or times out and the reason is in the application, not mirrord | `preview start` prints the last output from the preview pods alongside the error, which usually names the cause — a missing config file, a failed connection, a stack trace. If the job's log has scrolled away, `mirrord preview logs --key <key>` prints the same output for as long as the failed session is retained. |
| Preview pod never shows `Ready` | Intentional — mirrord inserts a readiness gate that never passes, so the target's Service doesn't route unfiltered traffic to the preview pod. See [Readiness](preview-environments.md#readiness). Use `mirrord preview status` to check the session's actual state. |
| Preview worked, then disappeared before the PR closed | The session's TTL elapsed. Re-run the workflow to recreate it, and consider a longer `ttl_mins`. |
| Requests with the header still hit the regular deployment | The header value must match the running session's key exactly — check `mirrord preview status` and make sure the config's `header_filter` uses the `{{ key }}` template rather than a hardcoded key. For requests made by backends (not the browser), confirm the header is [propagated](#header-propagation-for-backend-testing). |
