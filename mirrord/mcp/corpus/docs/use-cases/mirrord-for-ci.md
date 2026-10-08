---
title: mirrord for CI
date: 2025-10-20T21:00:00.000Z
lastmod: 2026-01-13T21:00:00.000Z
draft: false
menu:
  docs:
    parent: using-mirrord
toc: true
description: How to use mirrord in a CI environment with `mirrord ci` commands.
tags:
  - beta
  - oss
  - team
  - enterprise
---

# CI/CD Integration

mirrord can be used to greatly speed up CI runs by enabling testing against a shared staging environment without interrupting it. With mirrord for CI, you're able to keep your staging environment working, while also running your batch of end-to-end and other automated tests. The local app runs in the context of the targeted app that's deployed in your staging cluster, so it gets access to traffic, files, and more, as if it's running in the cluster. This means there's no need to spin up a whole test environment for a CI run, then spin it down when it's done.

![How mirrord ci works](../.gitbook/assets/mirrord-ci-diagram.png)

While running regular `mirrord exec` can be made to work for this purpose, it requires some finagling to get right, such as wrapping `mirrord exec` in some other command that would start it as a background process. The `mirrord ci start` command is more appropriate for this use case, since it starts your app and mirrord as background processes, allowing you to then run tests while your app is running in the background and connected to the cluster.

#### Prerequisites

1. Minimum mirrord CLI version `3.181.0`.
2. The CI runner must be able to access the Kubernetes cluster in which you want to test.

#### Kubernetes requirements

The CI runner must be able to access the Kubernetes cluster where the service you want to target is deployed, otherwise mirrord won't work. You'll need a [kubeconfig](https://kubernetes.io/docs/concepts/configuration/organize-cluster-access-kubeconfig/) in the CI runner that points to the target's cluster, and has the appropriate [authentication](https://kubernetes.io/docs/reference/access-authn-authz/authentication/).

It's recommended that you create a Kubernetes [service account](https://kubernetes.io/docs/concepts/security/service-accounts/) for the CI runner.

#### Using mirrord for CI on the Enterprise tier

{% hint style="info" %}
This section only applies to Enterprise plan users. If you're using the open-source version of mirrord, skip ahead to [Starting a mirrord CI session](#starting-a-mirrord-ci-session).
{% endhint %}

When the mirrord Operator is installed in the cluster, `mirrord ci` commands require a CI API key. Generating the key requires an Enterprise plan.
Without the key, every CI runner would register as a new user and take up a seat. Sessions started with a CI API key don't count towards your seats, so CI runs won't consume them.

You can get the key by running the command:

```sh
mirrord ci api-key
```

Copy it and save it as the **secret** environment variable `MIRRORD_CI_API_KEY` in your CI.

#### Starting a mirrord CI session

The `mirrord ci start` command is used to start the service being tested in your CI runner, and supports the same arguments as `mirrord exec`, including specifying a target with `--target` or using a configuration file with `--config-file`. Here’s an example for starting a Go service called `ip-visit-counter`:

{% hint style="info" %}
To view the entire list of arguments, run `mirrord ci start --help`.
{% endhint %}

```sh
mirrord ci start --target deployment/ip-visit-counter go run ip-visit-counter.go
```

At this point, your microservice will be running inside the CI runner and mirrord will be connecting it to the cluster. You can now run your test script as usual. These tests can target the stable, already deployed service in your cluster (for example, the service running in staging). mirrord will intercept that traffic and redirect it to the service running inside the CI runner, allowing it to work against real dependencies without deploying anything.

You can start multiple mirrord for CI sessions during a single CI job by running `mirrord ci start` more than once.

{% hint style="info" %}
If you want to run the service with mirrord in the foreground, you can use the `--foreground` arg.
{% endhint %}

#### Starting a mirrord CI session in a container

Use `mirrord ci container` when your CI job runs inside a local container runtime such as Docker.

```sh
mirrord ci container -- docker run my-image
```

**Application logs**

By default, `stdout` and `stderr` outputs from your application are saved to a file in the OS' temporary directory (e.g. `/tmp/mirrord`). A directory is created based on the name of the binary you're running with mirrord, for example:

```
mirrord ci start --target deployment/ip-visit-counter npm run

# /tmp/mirrord/npm-{unique run identifier}
```

You can change this directory with the config `ci.output_dir`:

```json
{
  "ci": {
    "output_dir": "/var/opt/mirrord"
  }
}
```

When running `mirrord ci start --config-file mirrord.json`, `stdout` and `stderr` outputs will be saved in the specified directory, e.g.:

```
mirrord ci start --config-file mirrord.json npm run

# /var/opt/mirrord/npm-{unique run identifier}
```

{% hint style="info" %}
When running with the --foreground argument, application logs are streamed to the foreground process’s `stdout` and `stderr` unless `ci.output_dir` is configured.
{% endhint %}

#### Stopping a mirrord CI session

After the tests are done, you should stop the mirrord CI session using `mirrord ci stop`. It's recommended that you do it, even if you won't be running mirrord for another service in this CI runner.

* Stopping a mirrord CI session:

```sh
mirrord ci stop
```

mirrord will stop running itself, and the local app.

{% hint style="info" %}
A single `mirrord ci stop` is enough to stop all running mirrord for CI sessions for that job, including sessions started with `mirrord ci container`.
{% endhint %}
