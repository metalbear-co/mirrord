---
title: "Cloudflare Containers"
description: "Add the mirrord remote bootstrap to a Cloudflare Container and connect it to the MetalBear Cloud sessions-manager, so developers can target it with mirrord."
tags:
  - alpha
  - team
---

# Cloudflare Containers

{% hint style="warning" %}
Targeting Cloudflare Containers is in alpha release status, and may change without notice. See [Required versions](README.md#required-versions) for the bootstrap image and CLI versions.
{% endhint %}

This guide makes a [Cloudflare Container](https://developers.cloudflare.com/containers/) targetable as a [workload outside Kubernetes](README.md). The container is connected to the [MetalBear Cloud sessions-manager](metalbear-cloud.md), so you don't need a Kubernetes cluster or the mirrord Operator. At a glance, the setup is:

1. Add the remote bootstrap to the container image ([Add the remote bootstrap](#add-the-remote-bootstrap)).
2. Store the API key as a Worker secret ([Store the API key](#store-the-api-key)).
3. Pass the bootstrap's variables to the container when the Durable Object starts it ([Start the container with mirrord](#start-the-container-with-mirrord)).
4. [Verify](#verify) that the container registers and a developer session works.

## Prerequisites

1. A Worker that runs your service in a Cloudflare Container, with a Durable Object class that starts the container and forwards requests to it.
2. An [API key](README.md#api-key), `<API_KEY>`.
3. An application image based on glibc 2.30 or later, and not on musl, such as Alpine. See [Application container requirements](README.md#application-container-requirements). Cloudflare Containers run `linux/amd64` images.
4. Wrangler, with permission to deploy the Worker and set its secrets.

Throughout this guide, replace:

| Placeholder | Value |
| --- | --- |
| `<YOUR_SERVICE_NAME>` | A name for the service you target, e.g. `payments` |
| `<YOUR_ENVIRONMENT>` | A name for the environment the service runs in, e.g. `staging` |
| `<YOUR_APPLICATION_IMAGE>` | Your application's existing image |

`<YOUR_SERVICE_NAME>` and `<YOUR_ENVIRONMENT>` are free-form: they're how a developer names the container they want. They must be identical on the container and in developers' `mirrord.json`.

---

## Add the remote bootstrap

Cloudflare Containers have no init containers or shared volumes, so the bootstrap library is copied into the application image at build time instead. Add these lines to your Dockerfile:

```dockerfile
FROM <YOUR_APPLICATION_IMAGE>

COPY --from=ghcr.io/metalbear-co/mirrord-remote-bootstrap:3.231.0 \
  /opt/mirrord/lib/libmirrord_remote_bootstrap.so \
  /opt/mirrord/lib/libmirrord_remote_bootstrap.so
```

The library is only loaded when `LD_PRELOAD` points to it, so the same image runs without mirrord when you don't set it. Bootstrap tags follow Operator release versions, not CLI versions; pin a release tag rather than `latest`.

The bootstrap verifies MetalBear's TLS certificates against the image's system trust store. Slim images, such as `node:22-bookworm-slim` or `debian:bookworm-slim`, ship without one. If `/etc/ssl/certs` is empty in your image, install `ca-certificates`, or copy the certificates from the full image:

```dockerfile
COPY --from=node:22-bookworm /etc/ssl/certs /etc/ssl/certs
```

Without them, the bootstrap logs `HTTP control plane request failed: builder error` and never registers.

## Store the API key

Store the API key as a [Worker secret](https://developers.cloudflare.com/workers/configuration/secrets/), so it isn't in your Wrangler configuration or source control:

```bash
wrangler secret put MIRRORD_SESSIONS_MANAGER_API_KEY
```

Paste `<API_KEY>` when prompted. Worker secrets aren't passed to the container automatically; the next step passes it on.

Put the names of the workload in the Worker's `vars`, in `wrangler.jsonc`:

```jsonc
{
  "vars": {
    "MIRRORD_REMOTE_SERVICE": "<YOUR_SERVICE_NAME>",
    "MIRRORD_REMOTE_ENVIRONMENT": "<YOUR_ENVIRONMENT>"
  }
}
```

## Start the container with mirrord

The container needs outbound internet access to reach MetalBear, and the bootstrap's variables in its environment. Set both where your Durable Object starts the container:

```typescript
container.start({
  enableInternet: true,
  env: {
    LD_PRELOAD: "/opt/mirrord/lib/libmirrord_remote_bootstrap.so",
    MIRRORD_REMOTE_SERVICE: this.env.MIRRORD_REMOTE_SERVICE,
    MIRRORD_REMOTE_ENVIRONMENT: this.env.MIRRORD_REMOTE_ENVIRONMENT,
    MIRRORD_REMOTE_SERVICE_REPLICA: this.ctx.id.toString(),
    MIRRORD_SESSIONS_MANAGER_API_KEY: this.env.MIRRORD_SESSIONS_MANAGER_API_KEY,
  },
});
```

If you use the `Container` class from `@cloudflare/containers`, set the same values in its `envVars` and `enableInternet` properties instead.

| Variable | Value |
| --- | --- |
| `LD_PRELOAD` | The path of the bootstrap library in the image, as above. |
| `MIRRORD_REMOTE_SERVICE` | `<YOUR_SERVICE_NAME>`. Developers target it as `serverless/<YOUR_SERVICE_NAME>`. |
| `MIRRORD_REMOTE_ENVIRONMENT` | `<YOUR_ENVIRONMENT>`. Developers set it as `target.namespace`. |
| `MIRRORD_REMOTE_SERVICE_REPLICA` | A name for this container instance, unique within the service. The Durable Object's ID is a good choice. |
| `MIRRORD_SESSIONS_MANAGER_API_KEY` | The API key, from the Worker secret. |

The bootstrap can't read a replica name from Cloudflare's metadata, as it does from ECS, and the container has no `HOSTNAME` to fall back on, so `MIRRORD_REMOTE_SERVICE_REPLICA` is required. Without it, the bootstrap logs `Missing serverless replica id`.

{% hint style="warning" %}
Don't set `MIRRORD_OPERATOR_API_URL`, `MIRRORD_OPERATOR_KUBE_CONTEXT` or `MIRRORD_SESSIONS_MANAGER_URL` on the container. They select a different sessions-manager; see [Connecting workloads](metalbear-cloud.md#connecting-workloads).
{% endhint %}

Deploy the Worker with `wrangler deploy`. A container that's already running keeps its old environment until it's restarted, for example when its inactivity timeout ends it.

The bootstrap's agent registers with sessions-manager while the container starts, which adds a few seconds to a cold start. If your Durable Object waits for the application's port before forwarding the first request, allow for that.

---

## Verify

1. **Container registered**: add `MIRRORD_AGENT_RUST_LOG: "info"` to the container's `env`. The application's output then shows `Registering with the MetalBear Cloud sessions-manager`, and no sessions-manager or API key errors.
2. **Developer session**: with the [developer setup](metalbear-cloud.md#developer-setup) and the `mirrord-up.yaml` from [Using mirrord with a Workload Outside Kubernetes](README.md#using-mirrord-with-a-workload-outside-kubernetes), with `common.operator: false`, `mirrord up` connects, and a request to the Worker with the `baggage` header reaches the local process:

   ```bash
   curl -H "baggage: mirrord-session=<YOUR_SESSION_KEY>" https://<YOUR_WORKER>.workers.dev/
   ```

   The same request without the header is still served by the container.

## Limitations

* **One replica per session.** A session is served by one container instance, the one whose `MIRRORD_REMOTE_SERVICE_REPLICA` it was paired with. If your Worker routes requests across several Durable Objects, only requests that reach that instance can be stolen or mirrored. For testing, route your test traffic to a single named instance, for example with `getByName`.
* **MetalBear Cloud only.** This guide covers the [MetalBear Cloud](metalbear-cloud.md) deployment mode. The [Operator-hosted](operator-hosted.md) mode isn't covered for Cloudflare Containers.

## Troubleshooting

The bootstrap's messages appear in the application container's output. For problems that don't depend on the platform, see [MetalBear Cloud Sessions-Manager](metalbear-cloud.md#troubleshooting).

| Symptom | Likely cause |
| --- | --- |
| `Missing serverless replica id` | `MIRRORD_REMOTE_SERVICE_REPLICA` isn't set on the container ([Start the container with mirrord](#start-the-container-with-mirrord)). |
| `HTTP control plane request failed: builder error` | The image has no CA certificates ([Add the remote bootstrap](#add-the-remote-bootstrap)). |
| A connection timeout to sessions-manager or `app.metalbear.com` | The container was started without `enableInternet: true`. |
| The container doesn't pick up a change to its variables | The running container keeps its environment. Restart it, for example by letting it reach its inactivity timeout. |
| The application container stops at startup: version `GLIBC_2.30` not found, required by `libmirrord_remote_bootstrap.so` | The application image's glibc is older than 2.30, or it's musl-based ([Prerequisites](#prerequisites)). |
