---
title: CLI
description: Installing and using the mirrord CLI
---

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
{% endtab %}

{% tab title="Windows" %}
To install the CLI, run:

```powershell
choco install --pre mirrord
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

Configuration options are documented in the [configuration reference](https://metalbear.com/mirrord/docs/config).

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
