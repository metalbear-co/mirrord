---
title: Install mirrord
description: "Install the mirrord CLI or the IDE extension on your machine, and check that your cluster meets the requirements."
tags:
  - oss
  - team
  - enterprise
---

# Install mirrord

Install the CLI, the extension for your IDE, or both.

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

{% tab title="VS Code / Cursor / Windsurf" %}

Extensions → search **mirrord** → Install.

Works with all VS Code forks (Cursor, Windsurf, Antigravity, PearAI, Trae).

You can also download it from the [VS Code Marketplace](https://marketplace.visualstudio.com/items?itemName=MetalBear.mirrord). For settings and usage, see [VS Code](../installing-mirrord/vscode.md).

{% endtab %}

{% tab title="JetBrains" %}

Preferences → Plugins → search **mirrord** → Install.

Works with IntelliJ, GoLand, PyCharm, and other JetBrains IDEs.

You can also download it from the [JetBrains Marketplace](https://plugins.jetbrains.com/plugin/19772-mirrord). For settings and usage, see [JetBrains IDEs](../installing-mirrord/intellij.md).

{% endtab %}
{% endtabs %}

## Check the install

- **CLI:** run `mirrord --version`. It prints the installed version.
- **VS Code:** a **mirrord** button with a circle appears in the status bar at the bottom of the window.
- **JetBrains:** the mirrord icon appears in the navigation toolbar, top right.

## Cluster Requirements

mirrord supports Kubernetes `1.22` or later. This applies to the open-source version and to the [mirrord Operator](../managing-mirrord/operator.md). The nodes where agents run need Linux kernel `5.4` or later, and a containerd, CRI-O or Docker runtime. Some features need a newer Kubernetes version, listed in [Which Kubernetes versions does mirrord support?](../faq/limitations.md#which-kubernetes-versions-does-mirrord-support)

## Next steps

- [Onboarding Wizard](onboarding-wizard.md): create your first mirrord config file.
- [Starting your first session](first-session.md): run your local code in the context of the cluster.
- [Install the Operator](install-the-operator.md): for teams sharing a cluster, the platform team installs it once per cluster.
