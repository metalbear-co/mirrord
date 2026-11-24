---
title: Sync Your Local Code to a Preview Environment
description: Preview a service without building an image, by syncing your local code into a preview pod with `mirrord preview sync`.
tags:
  - beta
  - enterprise
---

# Sync Your Local Code to a Preview Environment

`mirrord preview sync` previews your local code without building an image. It starts a preview pod from the target's image, or from an image that you choose, and syncs your local directories into it. With `--watch`, every change that you save is synced to the pod, and the app restarts when it needs to.

Code sync is part of [Preview Environments](preview-environments.md), so it works with the same keys, TTLs, traffic filters, queue splitting, and database branches. You set it up per service, in the `sync` section of [`mirrord-up.yaml`](../using-mirrord/multiple-concurrent-sessions.md#config-file-mirrord-up.yaml).

{% hint style="info" %}
This feature is available to users on the Enterprise pricing plan.
{% endhint %}

## Requirements

* mirrord `3.286.0` or later, and mirrord operator `3.231.0` or later with operator Helm chart `3.231.0` or later.
* Kubernetes `1.29` or later, on the API server and on the nodes. The preview pod runs a [native sidecar](https://kubernetes.io/docs/concepts/workloads/pods/sidecar-containers/), and the operator fails the preview with a message that names the version when either is older.
* A StorageClass for the code volume. On a cluster without a default StorageClass, your administrator sets one in the Helm values, or stores the code on an `emptyDir` instead (see [Operator Setup](#operator-setup)).
* Linux or macOS for the CLI.
* An image that can run your code. By default, the preview runs the image of the target's container, so that image needs the tools that your `prepare` and `run` commands use, such as `node` and `npm`.
* One preview pod. Leave `feature.preview.replicas` at `1`, the default.
* A target other than a CronJob.
* A single-cluster setup. Code sync doesn't run on a [multi-cluster](../using-mirrord/multi-cluster-setup.md) operator.

## Examples

### Next.js App

A Next.js dev server, which reloads changed files on its own:

```yaml
services:
  web:
    target:
      path: deployment/web
      namespace: staging
    run:
      command: ["npm", "run", "dev"]
    sync:
      files:
        - local: web
          remote: /app
          ignore: ["coverage/"]
      prepare:
        command: ["npm", "ci"]
        directory: /app
        triggered_by: ["web/package-lock.json"]
      run:
        command: ["npm", "run", "dev", "-H", "0.0.0.0", "-p", "3000"]
        directory: /app
```

```bash
mirrord preview sync --watch -k my-feature
```

This command:

1. Starts a preview pod for `web` from the image of `deployment/web`.
2. Syncs your local `web` directory to `/app` in the pod.
3. Runs `npm ci` in `/app`, and then starts `npm run dev`.
4. Keeps running, and syncs each change that you save. A change to `package-lock.json` runs `npm ci` again and restarts the app. Any other change only updates the files, and the dev server reloads them.

The top-level `run` is how `mirrord up` runs the service on your machine, and `sync.run` is how the preview pod runs it. Every service needs the top-level `run`, even one that you only preview with `mirrord preview sync`.

### Go Service

A Go service, which must restart to run new code:

```yaml
    sync:
      files:
        - local: api
          remote: /src
      run:
        command: ["go", "run", "."]
        directory: /src
        restart: on_sync
```

### Static Site

nginx serving a site that you build on your machine:

```yaml
    sync:
      files:
        - local: site/dist
          remote: /usr/share/nginx/html
```

Without `sync.run`, the pod runs the image's own command, so nginx serves the synced files as they change.

## Choosing the Services

`mirrord preview sync` previews every service that has `sync`, except the services with `skip: true`.

It takes the same options as `mirrord preview up` (`-f`, `--key`, `--ttl`, `--timeout`, and `--image`), and adds `--watch`.

### Images

A service with `sync` needs no image: by default, its preview runs the image of the target. To run another image, pass it the same way as for `mirrord preview up`, with `--image <service>=<image ref>` or `MIRRORD_<SERVICE>_IMAGE`. Your local directories are then synced into that image.

Passing an image for a service with `sync` and `skip: true` previews that service too.

When there is nothing to preview, the command fails before it changes the cluster. That happens when no service has `sync`, or when every service with `sync` has `skip: true` and no image.

### Switching Between `mirrord preview up` and `mirrord preview sync`

`mirrord preview up` ignores `sync`, and still needs an image for every service. Both commands use the same keys, so for one service and key, each command replaces the preview of the other:

* To go back to a regular preview of a service, run `mirrord preview up` with an image for it.
* To go back to code sync, run `mirrord preview sync`.

## How a Sync Works

A sync copies your `sync.files` directories to the preview pod. Each run of `mirrord preview sync` starts with a sync of all your files. With `--watch`, each later change triggers another sync.

### What Gets Synced

For each entry in `sync.files`, the CLI syncs everything in `local`, except:

* `.git` and `.jj`.
* The paths that git ignores. The CLI reads the same files as git: the `.gitignore` files of the repository, `.git/info/exclude`, and your global excludes file.
* The paths in the entry's `ignore` list, written as lines of a `.gitignore`, such as `coverage/`.

The `ignore` list takes precedence over git's rules. For example, `!.env.preview` in `ignore` syncs a file that a `.gitignore` excludes.
As in git, a `!` line can't bring back a file inside an ignored directory.

{% hint style="warning" %}
The ignore rules apply to every file, including the files that git tracks. Unlike in git, a tracked file that matches an ignore rule isn't synced.
{% endhint %}

### What Gets Sent

The CLI only sends the files that are new or changed, and the pod leaves unchanged files untouched. So a sync of all your files, such as the first sync of a run or the sync after a reconnect, is fast when the pod already has most of them.

### What a Sync Changes in the Pod

A sync makes each `remote` directory match its `local` directory:

* The files, directories, and symlinks in `local` are written to `remote`.
* The paths in `remote` that aren't in `local` are deleted, including files that came from the image.
* Ignored paths are left untouched, such as files from the image, or the `node_modules` that `prepare` installed.
* A directory that already exists keeps its permissions, because git doesn't record the permissions of directories.

A sync is applied completely or not at all. If it fails partway, the pod keeps the code of the last complete sync.

### Volumes Mounted Under a Synced Path

The preview pod is a copy of the target's pod, with the same volumes. A volume that the target mounts inside a `remote` directory hides the synced files at that path. For example, with `remote: /app` and a Secret mounted at `/app/.env`, the app reads the Secret's file, not your local `.env`.

The operator rejects a target that mounts a volume at a `remote` or `prepare.persist` directory, or at a directory above one.

## Running the App

### With `sync.run`

The pod runs `sync.run.command` instead of the container's own command:

* The command runs in `sync.run.directory`, or in the container's working directory if it isn't set.
* If the image doesn't have that directory, the pod creates it as the app's user. So an app that doesn't run as root needs a parent directory that it can write to.
* The pod removes the container's liveness and startup probes, because `prepare` and restarts of the app would fail them.

### Without `sync.run`

The container runs its own command, with its own working directory and probes, and the synced files change while it runs. Use this for an image that only needs the files, such as a web server for static files.

Without `sync.run`, the pod can't run `prepare`, so `sync.prepare` requires `sync.run`.

### What Happens After a Sync

![After each sync, the pod runs prepare if a triggered_by file changed, on first start, or after a failed prepare. A successful prepare restarts the app. A failed first prepare stops mirrord preview sync, and a later failure leaves the app running. Without prepare, the app restarts only with restart: on_sync.](../.gitbook/assets/preview-sync-flow.svg)

### Preparing the Code

`sync.prepare.command` prepares your code for the app, for example by installing dependencies. The pod runs it:

* Before it starts the app for the first time.
* After a sync that changes a file that `sync.prepare.triggered_by` matches.

The pod compares the contents of those files, so a sync that rewrites a file with the same contents doesn't run `prepare`.

`prepare` runs in `sync.prepare.directory`, or in the container's working directory if it isn't set.

After a failed `prepare`, the next sync runs it again, even when it reverts the `triggered_by` files. The CLI and `mirrord preview status` show the output of the failed `prepare`.

{% hint style="warning" %}
Everything that `prepare` writes must be on the code volume, because the pod skips `prepare` after a restart when nothing changed. Files under a `remote` directory, such as `/app/node_modules`, are already on the volume. List every other path that `prepare` writes to in `sync.prepare.persist`, such as `/usr/local/lib/python3.12/site-packages`, or its files are lost when the pod restarts.
{% endhint %}

{% hint style="info" %}
Keep the `triggered_by` globs narrow, such as `web/package-lock.json`. The pod checks every file that they match after each sync, so a glob that matches `node_modules` slows down every sync.
{% endhint %}

### Restarting the App

`sync.run.restart` controls when the app restarts:

| Value | The app restarts | Use it for |
|---|---|---|
| `on_prepare` (default) | After `prepare` runs | A dev server that reloads changed files on its own |
| `on_sync` | After every sync | An app that only reads its code when it starts, such as a compiled service |

{% hint style="info" %}
A restart only restarts the app's process in the running container, not the container or the pod, so it's quick.
{% endhint %}

## Watching for Changes

Without `--watch`, `mirrord preview sync` syncs your code once, and returns when the preview is ready. If the pod restarts before it's ready, the CLI syncs again.

With `--watch`, the CLI keeps running and syncs each change:

* It waits a moment, so that changes made together go in one sync.
* It only syncs the paths that changed.
* It skips changes in `.git` and in ignored paths.
* A change to a `.gitignore`, `.git/info/exclude`, or your global excludes file triggers a sync of all your files, so that the new rules apply everywhere.
* A change to `mirrord-up.yaml`, such as to `ignore`, applies the next time you run `mirrord preview sync`.

## Idle Mode

A preview with code sync can [scale to zero](preview-environments.md#auto-scaling-idle-mode) when it gets no traffic, as long as its code is on a PersistentVolumeClaim:

* A woken pod starts with the code of the last sync.
* With `--watch`, your first change wakes an idle preview.
* A sync or a `prepare` that is running keeps the preview from going idle.

A preview with code sync can't start idle, because the first sync needs a running pod. Set `sleep_after_secs` and leave `start_idle` unset: `mirrord preview sync` rejects `start_idle: true`.

On an `emptyDir`, the code would be lost when the preview scales to zero, so idle mode is off. `mirrord preview status` shows why.

## Operator Setup

Cluster administrators configure code sync under `operator.preview.codeSync` in the operator's Helm values:

```yaml
operator:
  preview:
    codeSync:
      storage:
        # `pvc` (the default) or `emptyDir`.
        kind: pvc
        # Unset uses the cluster's default StorageClass.
        storageClassName: standard-rwo
        # Defaults to 2Gi.
        size: 2Gi
      # Defaults to 65532. `null` sets none.
      fsGroup: 65532
      # Unset allows as much as the volume holds.
      maxPushBytes: 1073741824
      sidecarResources:
        requests:
          cpu: 10m
          memory: 32Mi
        limits:
          memory: 256Mi
```

* `storage.kind`: where each preview stores its code. A preview keeps the kind that it started with.
  * `pvc` gives each preview a PersistentVolumeClaim of its own. The code survives when the pod is replaced or scales to zero.
  * `emptyDir` needs no StorageClass, but the code is lost with the pod, and idle mode is off.
* `storage.storageClassName`: the StorageClass of the claim. When it's unset and the cluster has no default StorageClass, a preview with code sync fails with a message that names this setting.
* `storage.size`: the size of the claim, or the size limit of the `emptyDir`. A sync that changes most of your files needs room for the old and the new files at the same time, so use about twice the size of the synced files. The kubelet evicts a pod whose `emptyDir` goes past its limit.
* `fsGroup`: the group that a preview pod gets when nothing else names one, so that an app that doesn't run as root can write to the code volume.
  * When the pod has no `fsGroup`, the operator gives it the app container's `runAsGroup` or `runAsUser`, else the pod's `runAsGroup` or `runAsUser`, else this value.
  * The `fsGroup` applies to every volume of the pod, so the pod's Secret and ConfigMap files become readable by that group. An app that rejects a group-readable key file then fails in the preview.
* `maxPushBytes`: the most bytes, unpacked, that one sync may write. A larger sync is rejected before it changes anything, with a message that names this setting.
* `sidecarResources`: the resources of the `mirrord-sync` sidecar and its `mirrord-sync-install` init container. Set them when a namespace quota requires requests or limits on every container.

### Images

Each preview pod has a `mirrord-sync-install` init container, which runs the operator's image. It copies the sync sidecar into the pod, and with `sync.run`, the supervisor that runs your app.

The preview pod runs in the target's namespace, and pulls the operator's image with the target pod's pull secrets. So when you [mirror the operator image](../managing-mirrord/operator.md#using-an-internal-registry-optional) to a private registry, the target's pods must list a pull secret for that registry.

### Permissions

`mirrord preview sync` syncs through the `previews/sync` subresource of the operator's API. The operator chart's user and CI roles grant `get` on it. If you maintain your own roles, add this rule:

```yaml
- apiGroups:
    - operator.metalbear.co
  resources:
    - previews/sync
  verbs:
    - get
```

The developer needs no access to the preview pod itself. A sync connection uses a developer seat, as a mirrord session does.
