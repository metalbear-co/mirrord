---
title: Install mirrord
description: "Install the mirrord CLI on your machine, and check that your cluster meets the requirements."
tags:
  - oss
  - team
  - enterprise
---

# Install mirrord

To run mirrord from VS Code or a JetBrains IDE, see [Use mirrord in your IDE](use-mirrord-in-your-ide.md).

For every IDE, coding agent and tool that mirrord works with, see [mirrord integrations](https://metalbear.com/mirrord/integrations/).

## Requirements

- macOS (Intel or Apple Silicon), Linux (x86_64 or arm64), or Windows (x86_64). mirrord runs natively on Windows. If you prefer WSL, see the [WSL setup guide](../installing-mirrord/wsl.md).
- A kubeconfig that points at your cluster, the same one `kubectl` uses.
- A cluster that meets the [Cluster Requirements](#cluster-requirements).

## Install

{% tabs %}
{% tab title="CLI (macOS/Linux)" %}

```bash
brew install metalbear-co/mirrord/mirrord
```

or:

```bash
curl -fsSL https://raw.githubusercontent.com/metalbear-co/mirrord/main/scripts/install.sh | bash
```

To install a specific release and check the download against the checksum published with it, pass the version and set `MIRRORD_CHECKSUM` to the SHA-256 of the `mirrord_<os>_<arch>.zip` asset shown on the [release page](https://github.com/metalbear-co/mirrord/releases). This is useful in CI, where a changed binary should fail the build rather than run:

```bash
curl -fsSL https://raw.githubusercontent.com/metalbear-co/mirrord/main/scripts/install.sh \
  | MIRRORD_CHECKSUM=<sha256 of the zip> MIRRORD_INSTALL_DIR="$HOME/.local/bin" bash -s -- <version>
```

`MIRRORD_VERIFY=1` checks against the published checksum without pinning one, and `MIRRORD_INSTALL_DIR` picks the directory (no `sudo`). In GitHub Actions, [`metalbear-co/setup-mirrord`](https://github.com/metalbear-co/setup-mirrord) does the same and caches the binary between runs.

{% endtab %}

{% tab title="CLI (Windows)" %}

```powershell
choco install mirrord
```

{% endtab %}

{% endtabs %}

## Check the install

Run `mirrord --version`. It prints the installed version.

## Cluster Requirements

mirrord supports Kubernetes `1.22` or later. This applies to the open-source version and to the [mirrord Operator](../managing-mirrord/operator.md). The nodes where agents run need Linux kernel `5.4` or later, and a containerd, CRI-O or Docker runtime. Some features need a newer Kubernetes version, listed in [Which Kubernetes versions does mirrord support?](../faq/limitations.md#which-kubernetes-versions-does-mirrord-support)

## Next steps

- [mirrord MCP Server](../using-mirrord-with-ai/mcp-server.md): your AI agent generates mirrord configs, validates them deterministically, explains each option, and follows mirrord's skills and safe defaults, all offline.
- [Onboarding Wizard](onboarding-wizard.md): generate a `mirrord-up.yaml` for your services.
- [Starting your first session](first-session.md): run your local code in the context of your cluster or Amazon ECS.
- [Install the Operator](install-the-operator.md): for teams sharing a cluster, the platform team installs it once per cluster.
