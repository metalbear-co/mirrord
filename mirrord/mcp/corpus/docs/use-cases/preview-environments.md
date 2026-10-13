---
title: Preview Environments
tags:
  - beta
  - enterprise
lastmod: 2026-04-15T00:00:00.000Z
description: Ephemeral, isolated environments connected to your cluster
---

# Preview Environments

Preview Environments let teams collaborate, validate, and review new code using real traffic, without affecting live services.

A Preview Environment runs **only the new or changed services** in isolated pods inside your Kubernetes cluster. All other dependencies (for example, databases, queues, and upstream services) continue to run in the main cluster, such as staging, and are accessed via mirrord.

Because Preview Environments are not tied to a developer's local process, they are well suited for:

* Product managers exploring new features before they're merged
* QA engineers testing changes against realistic traffic and dependencies
* Engineers collaborating on a feature in progress or requesting async feedback

This enables realistic validation workflows without cloning an entire environment and without blocking on a single mirrord session. This model becomes even more valuable as [AI coding agents](../using-mirrord-with-ai/) begin shipping features and fixes autonomously. Instead of reviewing Git diffs alone, teams can let an AI agent deploy its changes into a preview environment automatically. The result is the service modified by the agent running in the cluster in isolation, while still connecting to its real dependencies inside the cluster. This allows teams to observe the code running, test end-to-end workflows, and validate behavior before anything is merged.

{% hint style="info" %}
This feature is available to users on the Enterprise pricing plan.
{% endhint %}

#### Prerequisites

* **An Enterprise license.** A [free trial](https://app.metalbear.com/account/sign-up) is minted as an Enterprise one, so preview environments work for its duration. An AI agent on a cluster with no license can provision that trial itself, without waiting for a person: see [Agent-Started Trials](../using-mirrord-with-ai/agent-started-trials.md).
* **Operator 3.142.0 or later** — the feature was introduced in this version.
* **CLI 3.189.0 or later** — the `mirrord preview` subcommand was introduced in this version.
*   **Helm flag** — `operator.previewEnv` must be `true` in your Helm values. It defaults to `true`, but older chart versions default to `false`, so set it explicitly when installing one of those:

    ```yaml
    operator:
      # Has to be set to `true` in order to use the preview environments feature.
      previewEnv: true
    ```

### What Is a Preview Environment?

Today, mirrord sessions are tightly coupled to a developer's local process. When that process stops, the testing environment disappears. Preview Environments solve this by allowing you to spin up isolated, temporary pods in the cluster that:

* Run **user-provided container images**
* Match the **configuration and traffic behavior** of an existing mirrord target
* Receive **filtered or duplicated staging traffic** using an environment key
* Stay alive for a **fixed TTL**, independent of any local machine or process

{% hint style="info" %}
With [multi-cluster](../using-mirrord/multi-cluster.md) mirrord, previews can run replicas on every workload cluster so traffic is served wherever it enters. See [Preview Environments in Multi-Cluster](../using-mirrord/multi-cluster.md#preview-environments-in-multi-cluster).
{% endhint %}

***

#### Environment Key

Each Preview Environment is identified by an **environment key**. The key is used to:

* Scope HTTP and queue traffic filtering
* Scope database branches
* Associate multiple preview pods into a single environment
* Share access to the same environment with other developers

If no key is provided, mirrord generates one automatically

***

### Starting a Preview Environment

Create a new Preview Environment using a mirrord configuration file and a container image:

```bash
mirrord preview start -f <mirrord.json> -i <image> -k <key>
```

Example output:

```
  ✓ mirrord preview start
    ✓ configuration loaded
    ✓ connected to operator
    ✓ preview session resource created
    ✓ preview pod is ready
  info:
    * key: <key>
    * namespace: <namespace>
    * session: preview-session-<target>-<id>
    * preview URL: https://<slug>.<shareDomain>
```

* If `-k` is omitted, mirrord generates a new key and prints it in the output.
* The image must be pullable from inside the cluster. The preview pod is a copy of the target's pod spec with the image swapped, so it pulls with the same credentials as the target — use a registry and repository the target already pulls from, otherwise the preview fails with `ErrImagePull`.
* The `preview URL` line only appears when [sharing via a link](#sharing-a-preview-via-a-link) is configured.

***

#### Managing Preview Environments

1. **Status:** Check the current state of Preview Environments, including which environments are active, which preview pods they contain, and how long they will remain available.

```bash
mirrord preview status
```

Add `--failed` to list the environments that failed instead of the active ones.

2. **Logs:** Print what a Preview Environment's pods have written. This is normally where the
reason for a failure lives — a missing config file, a failed connection, a stack trace from the
application itself:

```bash
mirrord preview logs --key <environment-key>
```

Pass `-t <target>` to read a single environment when several share a key. Failed environments are
included, and are retained for a short inspection window before they are cleaned up, so this
works for a while after a failure but not indefinitely. `mirrord preview start` prints the same
output at the moment it gives up.

`mirrord preview logs` requires mirrord `3.255.0` or later, and mirrord operator `3.205.0` or
later with operator Helm chart `3.205.0` or later. The operator serves the output, so an earlier
one cannot answer the command.

3. **Stop:** Manually remove a Preview Environment and its associated preview pods when it is no longer needed.

```bash
mirrord preview stop --key <environment-key>
```

4. **Replace:** Re-run `mirrord preview start` with the same key and target using `--force` (for example, after changing the image):

```bash
mirrord preview start -f <mirrord.json> -i <image> -k <key> --force
```

#### GitHub Action

We also provide the [`metalbear-co/mirrord-preview` GitHub Action](https://github.com/metalbear-co/mirrord-preview) for managing preview environments from your GitHub Actions pipeline. This can be used to, for example, automatically start a preview environment when a PR is opened and stop it when the PR is closed.

```yaml
name: Preview Environment
on:
  pull_request:
    types: [opened, synchronize, reopened, closed]

jobs:
  preview-start:
    if: github.event.action != 'closed'
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      # ... configure kubeconfig for your cluster ...
      - uses: metalbear-co/mirrord-preview@main
        with:
          action: start
          target: deployment/my-app
          namespace: staging
          image: myrepo/myapp:${{ github.sha }}
          filter: 'baggage: mirrord-session={{ key }}'
          key: pr-${{ github.event.repository.name }}-${{ github.event.pull_request.number }}

  preview-stop:
    if: github.event.action == 'closed'
    runs-on: ubuntu-latest
    steps:
      # ... configure kubeconfig for your cluster ...
      - uses: metalbear-co/mirrord-preview@main
        with:
          action: stop
          key: pr-${{ github.event.repository.name }}-${{ github.event.pull_request.number }}
```

Each PR gets an isolated preview keyed by its number. The `{{ key }}` template in the filter is replaced by mirrord with the session key at runtime, routing only matching traffic to the preview pod. When the PR is closed, the session is stopped and the preview pod is cleaned up. For the full list of inputs and configuration options, see the [action documentation](https://github.com/metalbear-co/mirrord-preview).

### Sharing a Preview via a Link

By default, opening a Preview Environment as a recipient requires the mirrord browser extension, which injects the `baggage: mirrord-session=<key>` header the operator routes on. That works for developers, but it's a non-starter for sharing a preview with a non-technical stakeholder.

`mirrord-share-ingress` moves that header injection off the client and onto a server-side component, so a plain HTTPS link works on its own with nothing to install on the recipient's side. Each shareable preview is reachable at its own host, `<slug>.<shareDomain>`, printed by `mirrord preview start` as the `preview URL`.

The `slug` mirrors the preview's key with a random suffix (for example `pr-myrepo-a1b2c3`), so the link is recognizable but unguessable; [stable share hosts](#stable-share-hosts) drop the suffix. When the session's TTL expires the host stops resolving, and the link falls through to a "preview not found" page. By default that page redirects to your app domain after a few seconds; with `shareIngress.notFound.redirect: false` it is a plain 404 (see [Setup](#setup)).

{% hint style="info" %}
The preview URL works with any HTTP filter. A preview with a custom filter (a path filter, a different header, composed filters) additionally routes requests carrying the share link's injected baggage header, so its own filter keeps working for regular traffic while the link always reaches the preview.
{% endhint %}

#### How it works

`mirrord-share-ingress` runs as its own Deployment and Service. It watches Preview Environments and, on each request, matches the request host to a live preview, injects `baggage: mirrord-session=<key>`, and forwards to that preview's target Service in-cluster. The operator's filtered steal at the target then routes the request to the preview pod, exactly as the browser extension's header would - the operator matches the injected header in addition to the session's own filter.

TLS and the public-facing ingress are owned by your platform team. You put an Ingress (or equivalent gateway) in front of the share-ingress Service that terminates TLS with a wildcard `*.<shareDomain>` certificate, preserves the `Host` header, and routes to the Service. Access control to the link is your responsibility as part of configuring that ingress.

#### Setup

1.  Configure the operator with the domain share hosts are minted under. This must match the `shareDomain` you give the share-ingress chart:

    ```yaml
    operator:
      previewEnv: true
      shareIngress:
        # Minted hosts look like <slug>.<shareDomain>. Enter without "*.".
        shareDomain: preview.example.com
    ```

2.  Install the `mirrord-share-ingress` chart. Install it **before** the first preview, since your ingress, DNS, and certificate point at its Service:

    ```bash
    helm install mirrord-share-ingress metalbear/mirrord-operator-share-ingress \
      --set shareIngress.shareDomain=preview.example.com \
      --set shareIngress.appDomain=example.com
    ```

    `appDomain` is where visitors land when a share link no longer resolves. A host with no live preview, such as an expired link or a mistyped host, gets a 404 page that redirects there after five seconds.

    To handle missing previews yourself instead, turn the redirect off (share-ingress chart 3.215.0 or later). The 404 page then only says "Preview not found", with no redirect, refresh, or JavaScript, and `appDomain` is not needed:

    ```bash
    helm install mirrord-share-ingress metalbear/mirrord-operator-share-ingress \
      --set shareIngress.shareDomain=preview.example.com \
      --set shareIngress.notFound.redirect=false
    ```

3.  Point a wildcard DNS record `*.preview.example.com` at your ingress, and create an Ingress with a wildcard certificate that routes to the share-ingress Service. A reference manifest (NGINX Ingress preserves the `Host` header by default):

    ```yaml
    apiVersion: networking.k8s.io/v1
    kind: Ingress
    metadata:
      name: mirrord-share-ingress
      namespace: mirrord
    spec:
      ingressClassName: nginx
      tls:
        - hosts:
            - "*.preview.example.com"
          secretName: share-ingress-tls
      rules:
        - host: "*.preview.example.com"
          http:
            paths:
              - path: /
                pathType: Prefix
                backend:
                  service:
                    name: mirrord-share-ingress
                    port:
                      number: 80
    ```

4.  Create the wildcard TLS secret the Ingress references, unless cert-manager (or your platform) already issues it:

    ```bash
    kubectl create secret tls share-ingress-tls \
      --cert=wildcard.crt --key=wildcard.key -n mirrord
    ```

#### Stable share hosts

By default the slug carries a random suffix, so the link only exists once `mirrord preview start` prints it. Set `operator.shareIngress.stableSlugs` when you need the link before that - for example when a PR bot posts the preview URL built from the PR number:

```yaml
operator:
  previewEnv: true
  shareIngress:
    shareDomain: preview.example.com
    stableSlugs: true
```

The host is then `<sanitized key>.<shareDomain>`: the key lowercased, every other character replaced with `-`, runs of `-` collapsed, and the whole label cut at 63 characters. A session with key `pr-myrepo-42` is reachable at `pr-myrepo-42.preview.example.com`.

Since anyone who knows the key can build the link, it is guessable. Only enable this when the ingress in front of `mirrord-share-ingress` authenticates every request.

A cluster allows one live session per host. Starting a second session whose key gives the same host fails right away with `share host <host> is already held by live preview session <namespace>/<name>`; stop that session or pick a different key. Failed sessions and sessions being deleted do not hold their host, so restarting a preview under the same key reuses the same link. Sessions that already have a host keep it when you turn the option on or off.

### Auto Scaling Idle Mode

Preview Environments can scale down to **zero pods while they receive no traffic**, then scale back up automatically when matching traffic arrives - without dropping that traffic. This makes long-lived
previews (for example, one per open PR) essentially free until someone actually uses them.

Enable it in the mirrord configuration:

```json
{
  "feature": {
    "preview": {
      "idle": {
        "start_idle": true,
        "sleep_after_secs": 300,
        "wake_timeout_secs": 90
      }
    }
  }
}
```

* `start_idle` - create the Preview Environment with zero pods. The first matching request or
  queue message starts the preview pods. `mirrord preview start` returns success as soon as the environment
  is ready to receive traffic, without waiting for a pod to run.
* `sleep_after_secs` - scale the preview pods to zero after this many seconds without traffic
  (minimum 30). When unset, the environment never idles automatically.
* `wake_timeout_secs` - How long an incoming request is held while the preview pods start before the request fails (default: 90 seconds).

#### Idle Behavior

An idle Preview Environment keeps listening: the traffic interception on the target and any
queue splits stay active even though the preview's pods are gone. When a request carrying the
environment's filter arrives, it is **held** while the pods start and answered by the preview
once ready - the caller just sees a slower first response. Queue messages don't need holding at
all: they wait in the environment's split queue/topic until the preview consumes them, so
nothing is lost either way.

While idle, `mirrord preview status` shows the environment as `idle (waiting for traffic)`,
and the session's TTL keeps counting. Idle mode requires a wake source - incoming traffic
enabled or queues split - since otherwise nothing could ever wake the environment.

Cluster administrators can cap how much traffic a waking environment may hold with these Helm
chart values (defaults: 512 protocol messages, 8 MiB of payload):

```yaml
operator:
  preview:
    idleHoldBufferMessages: 512
    idleHoldBufferBytes: 8388608
```

***

### Targeting Scaled-to-Zero Services

A Preview Environment that only splits queues can target a workload (Deployment, Argo Rollout,
or StatefulSet) with **no running pods**. (A [CronJob target](#targeting-cronjobs) never needs
running pods either.) This is useful when your consumers are auto-scaled on
queue lag (for example with KEDA) and sit at zero replicas until messages arrive. The split needs nothing from a live pod: topic and
consumer group are read from the workload's spec, and messages flow through the queue itself.
Matching messages reach the preview pod right away; unmatched ones wait on the target's
temporary queue and are consumed when the service scales back up, whose new pods start with the
split configuration already applied.

See [Autoscaled Targets with KEDA](../sharing-the-cluster/queue-splitting.md#autoscaled-targets-with-keda)
to see how KEDA autoscaling works with queue splitting.

A preview that also uses HTTP filtering or DB branching still needs a running target pod:
traffic is intercepted at the target's pods, and branch overrides are built from the env values
the running container sees. Such a session is rejected at creation with
`no Pod is ready to be a session target` - nothing partial is created. Idle mode (above) scales
the *preview's* pods to zero; this is about the *target's* pods, and the two combine freely.

***

### Targeting CronJobs

A Preview Environment can target a CronJob, so a flow that depends on a scheduled job (a
nightly scan, a report generator, a cleanup) can be previewed with your image too:

```bash
mirrord preview start -t cronjob/nightly-scan -i myrepo/scan:pr-4821 -k pr-4821 -f mirrord.json
```

Instead of a Deployment, the operator creates an isolated CronJob named after the session. It
copies the source CronJob's job settings (concurrency policy, history limits, deadlines, time
zone) and pod spec, swaps in your image, and applies the same environment overrides, database
branches, and file mounts any other preview gets. The copy is never suspended, even when the
source is, and the source CronJob is not modified. Kubernetes caps CronJob names at 52
characters, so when the session name is longer (a long source CronJob name pushes it there),
the preview CronJob gets a shortened name: the start of the session name plus the first 8
characters of the session's uid. Find it by its `preview.metalbear.co/session-uid` label; both
the session and its CronJob live in the target's namespace:

```bash
kubectl get cronjobs -n <namespace> -l preview.metalbear.co/session-uid=$(kubectl get previewsession <session> -n <namespace> -o jsonpath='{.metadata.uid}')
```

Right after creating it, the operator triggers the CronJob once, so you see a run immediately
instead of waiting for the next scheduled time. The run is a Job named `<cronjob>-start`,
marked with the `cronjob.kubernetes.io/instantiate: manual` annotation like a
`kubectl create job --from=cronjob/...` run. After that, the CronJob keeps running on its
schedule until the session ends, and every Job and pod it created is deleted with the session.

Set `feature.preview.cronjob.trigger_on_start` to `false` to skip that immediate run, for jobs
whose timing matters (a report that must only run in its window, a job that assumes the
previous scheduled run finished). The preview then runs on its schedule alone.

The schedule is inherited from the source CronJob. Override it with
`feature.preview.cronjob.schedule`, in Kubernetes CronJob syntax:

```json
{
  "target": "cronjob/nightly-scan",
  "feature": {
    "preview": {
      "image": "myrepo/scan:pr-4821",
      "cronjob": {
        "schedule": "*/30 * * * *",
        "trigger_on_start": true
      }
    }
  }
}
```

Omit `cronjob` (or set `schedule` to `null`) to keep the source schedule. A schedule that is
not five fields or a `@hourly`-style macro is rejected before anything is created; the API
server validates the field contents when the CronJob is created, and its message becomes the
session's failure message.

A CronJob preview has no long-running pod, so `feature.network.incoming` is ignored (with a
warning), `feature.preview.idle` is rejected, and `feature.preview.replicas` does not apply.
Database branch parameters with a `value_pattern` need a running target pod to read the
runtime value from, which a CronJob does not have, so they are rejected as well; plain
variable parameters work.

Requires operator 3.205.0 or later and CLI 3.256.0 or later, plus `create`, `delete`, and
`patch` on `batch/cronjobs` and `create` on `batch/jobs` for the operator, which the Helm chart
grants when `operator.previewEnv` is enabled.

***

### Building a Preview From Your Manifests

By default the preview pod is a copy of the target's live pod spec. Pass `--resource` to build
it from the Kubernetes manifests you already keep for the service instead. This is useful when
a pull request changes the Deployment or its ConfigMaps along with the code, and the preview
should run with those changes before they are deployed:

```bash
mirrord preview start -t deployment/app -i myrepo/app:pr-123 -k pr-123 --resource ./k8s/
```

`--resource` takes a file or a directory (only the `*.yaml` and `*.yml` files directly in it
are read) and can be repeated. The same list can live in the config as
`feature.preview.spec_resources`:

```json
{
  "target": "deployment/app",
  "key": "pr-123",
  "feature": {
    "preview": {
      "image": "myrepo/app:pr-123",
      "spec_resources": ["./k8s/app-deployment.yaml", "./k8s/configmap.yaml"]
    }
  }
}
```

mirrord only takes what can affect the preview pod from the files:

1. **The target itself.** Its pod template becomes the preview pod's spec, with your image.
   When the files do not define the target, the live one is used.
2. **The ConfigMaps and Secrets the pod uses**, through `env`, `envFrom`, or volumes.

Everything else in the files, such as an Ingress or another Deployment, is skipped, even when
it changed. Objects that are identical to what is live are skipped too:

```
Using deployment/app from ./k8s/app-deployment.yaml as the preview pod spec.
Applying 2 of 3 in-scope resources from ./k8s/ (1 unchanged, skipped):
deployment/app changed
configmap/app-config new
secret/app-tls unchanged, skipped
```

Live objects are never changed. A changed or new ConfigMap or Secret is created as a copy that
belongs to the preview, the preview pod uses the copy, and the copy is deleted when the preview
ends. The real app keeps using the originals. Secret values are sent to the operator the same
way as `secret_mounts`: they are stored in a Secret owned by the session, never on the
`PreviewSession` resource.

Before creating anything, mirrord checks the files:

* A file that is not valid YAML fails with its line and column, before the cluster is contacted.
* Every changed object is validated by the cluster with a dry run, which creates nothing. If
  the cluster rejects one, the command stops with the API server's reason and nothing is
  created. A preview that is already running with the same key is left as it is.

The comparison and the dry runs use your own credentials, not the operator's, so the operator
never reads your Secrets. For the full check you need:

* `get` on the target and on the ConfigMaps and Secrets its pod uses, to compare them.
* `create` on the target's kind, ConfigMaps, and Secrets, for the dry runs (they create
  nothing).

A CI identity that deploys the service usually has both. The `mirrord-operator-user` role alone
has neither. `--resource` still works without them: mirrord says which objects it could not
read or validate, uses the version from your files for those, and an object the cluster rejects
fails the preview when the operator creates it, instead of before anything is created.

Running `mirrord preview start` again with the same key replaces the preview, built from the
current files.

#### Comparing Your Manifests With the Cluster

`mirrord preview diff` runs the same checks and prints what differs for each object, without
creating anything:

```bash
mirrord preview diff -t deployment/app --resource ./k8s/
```

```
deployment/app from ./k8s/app-deployment.yaml: changed
  ~ spec.template.spec.containers[app].env[LOG_LEVEL].value
      - "info"
      + "debug"
configmap/app-config from ./k8s/configmap.yaml: new
  + data.FEATURE_FLAG: "on"
secret/app-tls from ./k8s/secret.yaml: changed
  ~ data.tls.key: (value hidden, changed)
ingress/web from ./k8s/ingress.yaml: outside the target's scope, skipped
```

Secret values are never printed, only whether each one changed.

`--resource` is not supported with a management-only multi-cluster operator yet, because the
target lives in another cluster than the one the CLI talks to. It requires CLI 3.266.0 or later
and operator 3.213.0 or later; with an older operator the CLI refuses `--resource` instead of
starting a preview from the live spec.

***

### Preview Environment Workflow

![Preview Environment Creation Workflow](../.gitbook/assets/create-env.svg)

![Preview Environment Modification Workflow](../.gitbook/assets/modify-env.svg)

***

### Details

#### Readiness

Pods created by Preview Environments will never be in the "Ready" state, this is intentional. mirrord inserts a [`readinessGate`](https://kubernetes.io/docs/concepts/workloads/pods/pod-lifecycle/#pod-readiness-gate) in the created pod that will never evaluate to `"True"` to prevent the target's `Service` from routing traffic to it, since that requires the pod to be ready. This allows the preview pod to copy all the labels/annotations present in the target's pod spec without worrying about the `Service`'s selector(s).

#### Service Meshes

On mesh-injected targets the preview pod gets a sidecar like any other pod, and the sidecar would normally capture the operator's incoming connections - the ones delivering the session's matched requests - and reject them (for example under `STRICT` mTLS). The operator therefore annotates the preview pod with [`traffic.sidecar.istio.io/excludeInboundPorts`](https://istio.io/latest/docs/reference/config/annotations/) (Istio) and [`config.linkerd.io/skip-inbound-ports`](https://linkerd.io/2/reference/proxy-configuration/) (Linkerd) for the session's subscribed ports. The sidecar stays in the pod, so the preview app's outgoing traffic still goes through the mesh, and ports already excluded on the target's template are preserved.

Only Istio and Linkerd are handled automatically. On another mesh (for example Kuma), the preview pod's sidecar still captures the operator's incoming connections and preview-matched requests fail. If you run a mesh we don't handle yet, please [reach out](https://metalbear.com/slack) so we can add support for it. In the meantime, if your mesh has an inbound-port-exclusion annotation, a cluster administrator can set it for all preview pods through the operator's preview pod configuration, pointing it at the ports your previews serve.

#### Resources

Preview Environments consist of a Deployment, to manage and maintain the underlying pods, and a [Headless Service](https://kubernetes.io/docs/concepts/services-networking/service/#headless-services), to route traffic to the dynamic set of pods. Because the Service doesn't have a Cluster IP, exhaustion of IP addresses when deploying a large number of Preview Environments is not a concern.

A preview of a [CronJob target](#targeting-cronjobs) consists of a CronJob and the Jobs it creates instead, with no Service.

#### Interaction with `mirrord exec`

If you start a local `mirrord exec` session against the same target and with the same Environment Key as an active Preview Environment, the local session takes precedence.

In that case, mirrord temporarily pauses the conflicting Preview Environment so the local session can receive the matching traffic. When the local session ends, the Preview Environment is resumed automatically.
