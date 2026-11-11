---
title: CLI
description: "How to install the mirrord CLI on your machine and run your first session from the terminal."
---

# CLI

The mirrord CLI is the core tool for running mirrord from the command line.

## Installation

{% tabs %}
{% tab title="MacOS/Linux" %}

To install the CLI, run:

```bash
brew install metalbear-co/mirrord/mirrord
```

or

```bash
curl -fsSL https://raw.githubusercontent.com/metalbear-co/mirrord/main/scripts/install.sh | bash
```

To install a specific release and check the download against the checksum published with it, pass the version and set `MIRRORD_CHECKSUM` to the SHA-256 of the `mirrord_<os>_<arch>.zip` asset shown on the [release page](https://github.com/metalbear-co/mirrord/releases). This is useful in CI, where a changed binary should fail the build rather than run:

```bash
curl -fsSL https://raw.githubusercontent.com/metalbear-co/mirrord/main/scripts/install.sh \
  | MIRRORD_CHECKSUM=<sha256 of the zip> MIRRORD_INSTALL_DIR="$HOME/.local/bin" bash -s -- 3.271.0
```

`MIRRORD_VERIFY=1` checks against the published checksum without pinning one, and `MIRRORD_INSTALL_DIR` picks the directory (no `sudo`). In GitHub Actions, [`metalbear-co/setup-mirrord`](https://github.com/metalbear-co/setup-mirrord) does the same and caches the binary between runs.
{% endtab %}

{% tab title="Windows" %}
To install the CLI, run:

```powershell
choco install mirrord
```
{% endtab %}
{% endtabs %}

## Usage

To use mirrord to plug a local process into a pod/deployment in the cluster configured with kubectl, run:

```bash
mirrord exec --target <target-path> <command used to run the local process>
```

For example:

```bash
mirrord exec --target pod/app-pod-01 python main.py
```

Use `mirrord --help` to see all available commands.

## Configuration

mirrord is configured using a JSON or YAML configuration file. The CLI reads its configuration from this file — by default, it looks for `.mirrord/mirrord.json` in the current directory. You can specify a different configuration file with the `-f` flag:

```bash
mirrord exec -f my-config.json --target pod/app-pod python main.py
```

Configuration options are documented in the [configuration reference](https://metalbear.com/mirrord/docs/config). For a user-wide Operator preference or default Kubernetes context, see [Global Configuration](global-configuration.md). Other settings in the global file are not yet applied.

## Interactive Setup

You can use `mirrord wizard` to generate a `mirrord.json` configuration file interactively. This walks you through common use cases and helps you create a configuration tailored to your needs. See the [Onboarding Wizard](../getting-started/onboarding-wizard.md) for more information.

## Listing Targets

To see available targets in your cluster:

```bash
mirrord ls
```

This will list all pods, deployments, and other resources that mirrord can target.

## Verifying Installation

To verify mirrord is installed correctly:

```bash
mirrord --version
```

To check connectivity to your cluster and the mirrord Operator (if using [mirrord for Teams](../managing-mirrord/operator.md)):

```bash
mirrord operator status
```

{% hint style="info" %}
**Got it working? Stuck?** Either way, [come say hi in Slack](https://metalbear.com/slack)
{% endhint %}
